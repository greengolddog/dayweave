package com.greengolddog.dayweave.data

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

class RoutinePlanningInputPersistenceTest {
    @get:Rule val temporary = TemporaryFolder()
    private val now = Instant.parse(ROUTINE_NOW).toEpochMilli() + 1
    private fun capsule(state: DayWeaveUiState) = RoutinePlanningInputCapsule.create(state,
        " \n" + ROUTINE_PLANNING_JSON.encodeToString(planningTestRequest()) + "\n", planningTestWitness(), ROUTINE_NOW)

    @Test fun encryptedCodecRestartRetainsFullFixedInputButNoRuntimeAdmission(): Unit = runBlocking {
        val directory = temporary.root.toPath().resolve("planning-input")
        fun repository(prepare: Boolean) = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-planning-binding", prepare))) { now }
        val base = planningTestReadyState(); val saved = capsule(base)
        repository(true).save(base.copy(routinePlanningInputCapsule = saved, routineOccurrenceAuthorityGeneration = 99))
        val encrypted = String(Files.readAllBytes(directory.resolve("snapshot.aesgcm")), Charsets.UTF_8)
        assertFalse(encrypted.contains("Synthetic private note")); assertFalse(encrypted.contains(saved.originalRequestJson))
        val restored = requireNotNull(repository(false).load())
        assertEquals(saved, restored.routinePlanningInputCapsule)
        assertEquals(0L, restored.routineOccurrenceAuthorityGeneration)
        assertNull(restored.routineOccurrenceDeferAdmission)
        assertTrue(restored.itemCompletionGetProofs.isEmpty())
        assertNull(restored.localScheduleCompositionProvenance)
        assertTrue(restored.routineOccurrenceLedger.observations.isEmpty())
        assertEquals(3, saved.canonicalItems.size)
        assertEquals(3, saved.witness.occurrenceLifecycle.instances.single().members.size)
        assertEquals(base.routineOccurrenceLedger, restored.routineOccurrenceLedger)
    }

    @Test fun v24MigrationPreservesEveryPriorSerializedFieldAndAllRecoveryCustody(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        // Exact object comparison below covers every pre-existing field, not just these sidecars.
        val base = completionTestState(false).copy(
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation(true))),
            itemCompletionLedger = completionTestLedger().copy(pending = listOf(completionTestMutation(true))),
            routineOccurrenceLedger = routineStateTestLedger().copy(pending = listOf(routineStateTestIntent(true))))
        // Independent fixtures share this canonical binding only after an explicit fixture join.
        val original = base.copy(routineOccurrenceLedger = base.routineOccurrenceLedger.copy(
            syncOrigin = base.canonicalSyncOrigin, configurationId = base.canonicalConfigurationId,
            pending = base.routineOccurrenceLedger.pending.map { it.copy(syncOrigin = requireNotNull(base.canonicalSyncOrigin), configurationId = requireNotNull(base.canonicalConfigurationId)) }))
        repository.save(original)
        val modern = requireNotNull(dao.snapshot)
        val oldRoot = Json.parseToJsonElement(modern.payload).jsonObject - "routinePlanningInputCapsule" - "routinePlanningDisplaySnapshot"
        dao.snapshot = modern.copy(payloadFormat = PlannerSnapshotFormats.JSON_V24, payload = JsonObject(oldRoot).toString())
        val restored = requireNotNull(repository.load())
        assertNull(restored.routinePlanningInputCapsule)
        assertEquals(original.itemProgressLedger, restored.itemProgressLedger)
        assertEquals(original.itemCompletionLedger, restored.itemCompletionLedger)
        assertEquals(original.routineOccurrenceLedger, restored.routineOccurrenceLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V26, dao.snapshot?.payloadFormat)
        assertEquals(oldRoot, Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject - "routinePlanningInputCapsule" - "routinePlanningDisplaySnapshot")
    }

    @Test fun allPredecessorLabelsRejectInjectedCapsuleEvenNullWithoutRewriting(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        repository.save(planningTestReadyState()); val original = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(original.payload).jsonObject - "routinePlanningDisplaySnapshot"
        for (version in 1..24) for (value in listOf(JsonNull, ROUTINE_CAPSULE_JSON.encodeToJsonElement(capsule(planningTestReadyState())))) {
            val format = PlannerSnapshotFormats::class.java.getDeclaredField("JSON_V$version").get(null) as String
            val injected = original.copy(payloadFormat = format, payload = JsonObject(root + ("routinePlanningInputCapsule" to value)).toString())
            dao.snapshot = injected
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(injected, dao.snapshot)
        }
    }

    @Test fun currentSchemaRequiresExplicitCapsuleAndRejectsUnknownRuntimeFieldsOrBindingTampering(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        val base = planningTestReadyState(); repository.save(base.copy(routinePlanningInputCapsule = capsule(base)))
        val original = requireNotNull(dao.snapshot); val root = Json.parseToJsonElement(original.payload).jsonObject
        val raw = root.getValue("routinePlanningInputCapsule").jsonObject
        val badDigest = JsonObject(raw + ("originalRequestDigest" to JsonPrimitive("routine-input-request-sha256:" + "0".repeat(64))))
        val variants = listOf(JsonObject(root - "routinePlanningInputCapsule").toString(),
            JsonObject(root + ("routinePlanningInputAdmission" to JsonNull)).toString(),
            JsonObject(root + ("canonicalConfigurationId" to JsonPrimitive("other-binding"))).toString(),
            JsonObject(root + ("routinePlanningInputCapsule" to JsonObject(raw + ("live" to JsonPrimitive(true))))).toString(),
            JsonObject(root + ("routinePlanningInputCapsule" to badDigest)).toString(),
            "{\"routinePlanningInputCapsule\":null," + original.payload.drop(1))
        for (payload in variants) {
            val bad = original.copy(payload = payload); dao.snapshot = bad
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(bad, dao.snapshot)
        }
    }

    @Test fun exactCaptureCommitAndSameCursorPrivacyAbaNeverRestoreOperationPermission(): Unit = runBlocking {
        val store = PlannerStore(planningTestReadyState(), nowEpochMillis = { now })
        val fence = requireNotNull(store.captureRoutinePlanningInputFence()); val saved = capsule(fence.state)
        val committed = requireNotNull(store.commitRoutinePlanningInputCapsule(saved, fence) { true })
        assertTrue(committed.persistence.awaitDurable())
        assertTrue(store.isRoutinePlanningInputFenceCurrent(committed.postSaveFence))
        assertFalse(store.isRoutinePlanningInputFenceCurrent(fence))
        val before = requireNotNull(store.captureRoutinePlanningInputFence())
        store.invalidateRoutineOccurrenceAuthority(); store.invalidateRoutineOccurrenceAuthority()
        assertEquals(before.state.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
        assertEquals(before.stableInputFingerprint, store.state.value.routinePlanningStableInputFingerprint())
        assertFalse(store.isRoutinePlanningInputFenceCurrent(before))
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) { store.commitRoutinePlanningInputCapsule(saved, before) { true } }
        assertEquals(saved, store.state.value.routinePlanningInputCapsule)
        assertNull(store.state.value.localScheduleCompositionProvenance)
    }

    @Test fun privacyWithdrawalDuringAdmissionWorkCannotAcquireCustody() {
        val store = PlannerStore(planningTestReadyState(), nowEpochMillis = { now })
        val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        val saved = capsule(fence.state)
        var checks = 0
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) {
            store.commitRoutinePlanningInputCapsule(saved, fence) { ++checks == 1 }
        }
        assertEquals(2, checks)
        assertSame(fence.state, store.state.value)
        assertNull(store.state.value.routinePlanningInputCapsule)
        assertTrue(store.isRoutinePlanningInputFenceCurrent(fence))
    }

    @Test fun rejectedOwnerPinAndPrivateOperationDoNotReplacePreviouslySavedInput(): Unit = runBlocking {
        val base = planningTestReadyState(); val saved = capsule(base)
        val store = PlannerStore(base.copy(routinePlanningInputCapsule = saved), nowEpochMillis = { now })
        val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        val other = RoutinePlanningInputCapsule.create(fence.state, saved.originalRequestJson,
            saved.witness.copy(workspaceId = PLANNING_OWNER, userId = PLANNING_WORKSPACE), ROUTINE_NOW)
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) { store.commitRoutinePlanningInputCapsule(other, fence) { true } }
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) { store.commitRoutinePlanningInputCapsule(saved, fence) { false } }
        assertEquals(saved, store.state.value.routinePlanningInputCapsule)
        assertTrue(requireNotNull(store.abandonCanonicalConnection()).awaitDurable())
        assertNull(store.state.value.routinePlanningInputCapsule)
    }

    @Test fun failedReplacementSaveRollsBackToPriorDurableArtifactAndExactLedgers(): Unit = runBlocking {
        val base = planningTestReadyState(); val prior = capsule(base)
        val initial = base.copy(routinePlanningInputCapsule = prior)
        val repository = object : PlannerStateRepository {
            override suspend fun load() = initial
            override suspend fun save(state: DayWeaveUiState) { throw IOException("Synthetic disk failure") }
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(initial, repository, scope, nowEpochMillis = { now })
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val fence = requireNotNull(store.captureRoutinePlanningInputFence())
            val replacement = RoutinePlanningInputCapsule.create(fence.state, prior.originalRequestJson + " ", prior.witness, ROUTINE_NOW)
            val change = requireNotNull(store.commitRoutinePlanningInputCapsule(replacement, fence) { true })
            assertFalse(withTimeout(3_000) { change.persistence.awaitDurable() })
            assertEquals(prior, store.state.value.routinePlanningInputCapsule)
            assertEquals(initial.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
            assertFalse(store.isRoutinePlanningInputFenceCurrent(change.postSaveFence))
        } finally { scope.cancel() }
    }

    @Test fun prospectiveCombinedBudgetRejectsAtomicallyWithoutTrimmingSourceOrJournals() {
        // Individually valid capsule + state, but duplicated private source exceeds combined admission.
        val base = planningTestReadyState().let { it.copy(canonicalItems = it.canonicalItems.mapIndexed { index, item ->
            if (index == 0) item.copy(notes = "x".repeat(8 * 1024 * 1024)) else item }) }
        val store = PlannerStore(base, nowEpochMillis = { now }); val fence = requireNotNull(store.captureRoutinePlanningInputFence())
        val saved = capsule(fence.state); saved.requireValid()
        assertThrows(RoutinePlanningWitnessProtocolException::class.java) { store.commitRoutinePlanningInputCapsule(saved, fence) { true } }
        assertSame(fence.state, store.state.value)
        assertNull(store.state.value.routinePlanningInputCapsule)
        assertEquals(base.canonicalItems, store.state.value.canonicalItems)
    }

    @Test fun unchangedInertCapsuleDoesNotImposeNewGlobalLimitOnLaterOutboxPersistence(): Unit = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { now }
        val base = planningTestReadyState().copy(canonicalSyncOrigin = PROGRESS_ORIGIN, canonicalConfigurationId = PROGRESS_CONFIGURATION,
            routineOccurrenceLedger = planningTestReadyState().routineOccurrenceLedger.copy(syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION))
        val saved = capsule(base); repository.save(base.copy(routinePlanningInputCapsule = saved))
        val enlarged = base.copy(routinePlanningInputCapsule = saved,
            inbox = listOf(InboxItem("synthetic-large-existing-draft", title = "Synthetic", source = InboxSource.QUICK_CAPTURE,
                detail = "x".repeat(MAX_ROUTINE_PLANNING_CAPSULE_SNAPSHOT_ADMISSION_BYTES))),
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation(submitted = true, sensitive = true))))
        repository.save(enlarged)
        assertTrue(requireNotNull(dao.snapshot).payload.toByteArray().size > MAX_ROUTINE_PLANNING_CAPSULE_SNAPSHOT_ADMISSION_BYTES)
        val restored = requireNotNull(repository.load())
        assertEquals(saved, restored.routinePlanningInputCapsule)
        assertEquals(enlarged.itemProgressLedger.pending, restored.itemProgressLedger.pending)
        assertFalse(saved.isReusableInput(restored, now, true))
    }

    private class RawDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int) = snapshot
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }
}
