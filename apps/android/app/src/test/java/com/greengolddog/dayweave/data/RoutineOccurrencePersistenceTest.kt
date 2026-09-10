package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.sync.NativeConvergenceDisk
import com.greengolddog.dayweave.sync.NativeConvergenceSnapshotDao
import java.io.IOException
import java.nio.file.Files
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class RoutineOccurrencePersistenceTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun encryptedRestartKeepsExactRequestBytesAndHistoricalReceiptCatchUpCustody() = runBlocking {
        val directory = temporary.root.toPath().resolve("routine-restart")
        fun repository(prepare: Boolean) = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-routine-binding", prepare))) { 1_000 }
        val intent = routineStateTestIntent(true)
        val original = routineStateTestUi(routineStateTestLedger().copy(pending = listOf(intent)))
        repository(true).save(original)
        val ciphertext = Files.readAllBytes(directory.resolve("snapshot.aesgcm")).toString(Charsets.UTF_8)
        assertFalse(ciphertext.contains(intent.requestJson))
        assertFalse(ciphertext.contains("routineOccurrenceLedger"))
        val restored = requireNotNull(repository(false).load())
        assertEquals(original.routineOccurrenceLedger, restored.routineOccurrenceLedger)
        assertTrue(restored.canonicalItems.isEmpty())
        val newer = routineTestSnapshot(true).let { it.copy(aggregate = it.aggregate.copy(revision = 3)) }
        val observed = restored.routineOccurrenceLedger.observeRoutineOccurrence(RoutineOccurrenceObservation(newer, ROUTINE_NOW))
        val settled = restored.copy(routineOccurrenceLedger = observed.settleRoutineOccurrence(intent, routineTestMutation(true), ROUTINE_NOW))
        repository(false).save(settled)
        val receiptRestart = requireNotNull(repository(false).load())
        assertEquals(settled.routineOccurrenceLedger, receiptRestart.routineOccurrenceLedger)
        assertEquals(newer, receiptRestart.routineOccurrenceLedger.observations[ROUTINE_INSTANCE]?.snapshot)
        assertEquals(mapOf(ROUTINE_INSTANCE to 2L), receiptRestart.routineOccurrenceLedger.minimumCatchUpRevisions)
        assertTrue(PlannerStore(receiptRestart).hasCredentialReplacementBlocker())
    }

    @Test fun everyDispositionAndExactWhitespaceSurvivesRestartWithoutCanonicalCache() = runBlocking {
        for (disposition in RoutineOccurrenceDisposition.entries) {
            val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
            val original = routineStateTestUi(routineStateTestLedger().copy(pending = listOf(
                routineStateTestIntent(true).copy(disposition = disposition))))
            repository.save(original)
            assertEquals(PlannerSnapshotFormats.JSON_V25, dao.snapshot?.payloadFormat)
            val restored = requireNotNull(repository.load())
            assertEquals(original.routineOccurrenceLedger, restored.routineOccurrenceLedger)
            assertEquals(original.routineOccurrenceLedger.pending.single().requestJson,
                restored.routineOccurrenceLedger.pending.single().requestJson)
            assertTrue(restored.canonicalItems.isEmpty())
            assertFalse(requireNotNull(dao.snapshot).payload.contains("routineOccurrenceGetProofs"))
        }
    }

    @Test fun v23MigrationPreservesAllExistingJournalsAndAddsOnlyEmptyOccurrenceLedger() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        val original = completionTestState(false).copy(
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation(true))),
            itemCompletionLedger = completionTestLedger().copy(pending = listOf(completionTestMutation(true))))
        repository.save(original)
        val saved = requireNotNull(dao.snapshot)
        val oldRoot = Json.parseToJsonElement(saved.payload).jsonObject - "routineOccurrenceLedger" - "routinePlanningInputCapsule"
        val old = saved.copy(payloadFormat = PlannerSnapshotFormats.JSON_V23, payload = JsonObject(oldRoot).toString())
        dao.snapshot = old
        val restored = requireNotNull(repository.load())
        assertEquals(RoutineOccurrenceLedger(), restored.routineOccurrenceLedger)
        assertEquals(original.itemProgressLedger.pending, restored.itemProgressLedger.pending)
        assertEquals(original.itemCompletionLedger.pending, restored.itemCompletionLedger.pending)
        assertEquals(oldRoot, Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject - "routineOccurrenceLedger" - "routinePlanningInputCapsule")
        assertEquals(PlannerSnapshotFormats.JSON_V25, dao.snapshot?.payloadFormat)
    }

    @Test fun everyOlderFormatRejectsInjectedLedgerAndRuntimeProofEvenNullBeforeMigration() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(DayWeaveUiState())
        val original = requireNotNull(dao.snapshot)
        val currentRoot = Json.parseToJsonElement(original.payload).jsonObject - "routinePlanningInputCapsule"
        for (version in 1..24) {
            val format = PlannerSnapshotFormats::class.java.getDeclaredField("JSON_V$version").get(null) as String
            val root = currentRoot - if (version < 24) setOf("routineOccurrenceLedger") else emptySet()
            val withoutCompletion = root - if (version < 23) setOf("itemCompletionLedger") else emptySet()
            val historical = withoutCompletion - if (version < 22) setOf("itemProgressLedger") else emptySet()
            val forbidden = listOf("routineOccurrenceGetProofs", "routineOccurrenceReadProofs", "routineOccurrenceEvidenceGeneration",
                "routineOccurrenceReviewLease") + if (version < 24) listOf("routineOccurrenceLedger") else emptyList()
            for (field in forbidden) for (value in listOf(JsonNull, JsonObject(emptyMap()))) {
                val injected = original.copy(payloadFormat = format, payload = JsonObject(historical + (field to value)).toString())
                dao.snapshot = injected
                assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
                assertEquals(injected, dao.snapshot)
            }
        }
    }

    @Test fun v24RequiresEveryExplicitLedgerJournalAndNullableObservationField() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(routineStateTestUi(routineStateTestLedger().copy(pending = listOf(routineStateTestIntent()))))
        val original = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(original.payload).jsonObject
        val ledger = root.getValue("routineOccurrenceLedger").jsonObject
        val journal = ledger.getValue("pending").jsonArray.single().jsonObject
        val observation = ledger.getValue("observations").jsonObject.getValue(ROUTINE_INSTANCE).jsonObject
        fun withLedger(value: JsonObject) = JsonObject(root + ("routineOccurrenceLedger" to value))
        val variants = listOf(JsonObject(root - "routineOccurrenceLedger"), JsonObject(root + ("routineOccurrenceLedger" to JsonNull))) +
            ledger.keys.map { field -> withLedger(JsonObject(ledger - field)) } +
            journal.keys.map { field -> withLedger(JsonObject(ledger + ("pending" to JsonArray(listOf(JsonObject(journal - field)))))) } +
            observation.keys.map { field ->
                val observations = JsonObject(mapOf(ROUTINE_INSTANCE to JsonObject(observation - field)))
                withLedger(JsonObject(ledger + ("observations" to observations)))
            }
        for (variant in variants) {
            val malformed = original.copy(payload = variant.toString()); dao.snapshot = malformed
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(malformed, dao.snapshot)
        }
    }

    @Test fun v24RejectsUnknownFieldsDuplicateKeysTypedRequestChangesAndCrossOriginBinding() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        val originalState = routineStateTestUi(routineStateTestLedger().copy(pending = listOf(routineStateTestIntent())))
        assertThrows(SerializationException::class.java) { runBlocking { repository.save(originalState.copy(canonicalSyncOrigin = "other-origin")) } }
        repository.save(originalState)
        val original = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(original.payload).jsonObject
        val ledger = root.getValue("routineOccurrenceLedger").jsonObject
        val journal = ledger.getValue("pending").jsonArray.single().jsonObject
        val changedRequest = JsonObject(journal.getValue("request").jsonObject + ("expected_instance_revision" to JsonPrimitive(2)))
        val changedJournal = JsonArray(listOf(JsonObject(journal + ("request" to changedRequest))))
        val variants = listOf(
            JsonObject(root + ("canonicalConfigurationId" to JsonPrimitive("other-config"))).toString(),
            JsonObject(root + ("routineOccurrenceLedger" to JsonObject(ledger + ("getProof" to JsonNull)))).toString(),
            JsonObject(root + ("routineOccurrenceLedger" to JsonObject(ledger + ("pending" to changedJournal)))).toString(),
            "{\"routineOccurrenceLedge\\u0072\":$ledger," + original.payload.drop(1),
        )
        for (variant in variants) {
            val malformed = original.copy(payload = variant); dao.snapshot = malformed
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(malformed, dao.snapshot)
        }
    }

    @Test fun failedStageSubmissionAndReceiptSaveRestoreExactLastDurableLedgerAtomically() = runBlocking {
        for (stage in listOf("stage", "submission", "receipt")) {
            val intent = routineStateTestIntent(stage == "receipt")
            val initial = routineStateTestUi(routineStateTestLedger().copy(pending = if (stage == "stage") emptyList() else listOf(intent)))
            val repository = object : PlannerStateRepository {
                override suspend fun load() = initial
                override suspend fun save(state: DayWeaveUiState) { throw IOException("Synthetic occurrence disk failure") }
            }
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
            try {
                val store = PlannerStore(initial, repository, scope)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
                val save = store.mutateRoutineOccurrences { current -> when (stage) {
                    "stage" -> current.routineOccurrenceLedger.enqueueRoutineOccurrence(intent)
                    "submission" -> current.routineOccurrenceLedger.markRoutineOccurrenceSubmitted(intent, ROUTINE_NOW)
                    else -> current.routineOccurrenceLedger.settleRoutineOccurrence(intent, routineTestMutation(true), ROUTINE_NOW)
                } }
                assertFalse(withTimeout(3_000) { requireNotNull(save).awaitDurable() })
                assertEquals(initial.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
                withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.PERSISTENCE_FAILED } }
            } finally { scope.cancel() }
        }
    }

    @Test fun credentialReplacementAndQuarantineKeepEveryPendingDispositionAndReceiptLatch() = runBlocking {
        for (disposition in RoutineOccurrenceDisposition.entries) {
            val state = routineStateTestUi(routineStateTestLedger().copy(pending = listOf(routineStateTestIntent(true).copy(disposition = disposition))))
            val store = PlannerStore(state)
            assertTrue(store.hasCredentialReplacementBlocker())
            assertThrows(IllegalArgumentException::class.java) { store.abandonCanonicalConnection() }
            assertThrows(IllegalArgumentException::class.java) { store.quarantineRoutineOccurrenceLedger() }
            assertEquals(state.routineOccurrenceLedger, store.state.value.routineOccurrenceLedger)
        }
        val ledger = routineStateTestLedger().copy(needsRemoteScheduleCatchUp = true)
        assertTrue(PlannerStore(routineStateTestUi(ledger)).hasCredentialReplacementBlocker())
        val store = PlannerStore(routineStateTestUi())
        assertTrue(requireNotNull(store.abandonCanonicalConnection()).awaitDurable())
        assertEquals(RoutineOccurrenceLedger(), store.state.value.routineOccurrenceLedger)
    }

    private class RawDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int) = snapshot
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }
}
