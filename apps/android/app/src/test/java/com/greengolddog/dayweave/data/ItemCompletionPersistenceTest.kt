package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.sync.NativeConvergenceDisk
import com.greengolddog.dayweave.sync.NativeConvergenceSnapshotDao
import com.greengolddog.dayweave.state.PlannerStore
import java.nio.file.Files
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.Rule
import org.junit.rules.TemporaryFolder

class ItemCompletionPersistenceTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun authenticatedEncryptedRestartRetainsExactLedgerAndReceiptCatchUpCustody() = runBlocking {
        val directory = temporary.root.toPath().resolve("completion-restart")
        fun repository(prepare: Boolean) = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(
            NativeConvergenceDisk(directory, "synthetic-completion-binding", prepare))) { 1_000 }
        val original = completionTestState().copy(itemCompletionLedger = completionTestLedger().copy(
            pending = listOf(completionTestMutation(true))))
        repository(true).save(original)
        val ciphertext = Files.readAllBytes(directory.resolve("snapshot.aesgcm")).toString(Charsets.UTF_8)
        assertFalse(ciphertext.contains(completionTestMutation().requestJson))
        assertFalse(ciphertext.contains("itemCompletionLedger"))
        val restored = requireNotNull(repository(false).load())
        assertEquals(original.itemCompletionLedger, restored.itemCompletionLedger)
        assertTrue(restored.itemCompletionGetProofs.isEmpty())
        val settled = restored.copy(itemCompletionLedger = restored.itemCompletionLedger.settleItemCompletion(
            restored.itemCompletionLedger.pending.single(), completionTestResult(true), PROGRESS_NOW))
        repository(false).save(settled)
        val receiptRestart = requireNotNull(repository(false).load())
        assertTrue(receiptRestart.itemCompletionLedger.pending.isEmpty())
        assertTrue(receiptRestart.itemCompletionLedger.needsCanonicalCatchUp)
        assertTrue(PlannerStore(receiptRestart).hasCredentialReplacementBlocker())
    }

    @Test fun exactBytesAndHistoricalObservationSurviveRestartWithoutReadPermission() = runBlocking {
        for (disposition in ItemCompletionDisposition.entries) {
            val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
            val original = completionTestState().copy(canonicalItems = emptyList(), itemCompletionLedger = completionTestLedger().copy(
                pending = listOf(completionTestMutation(true).copy(disposition = disposition, wasSensitive = true)), needsCanonicalCatchUp = true))
            repository.save(original)
            assertEquals(PlannerSnapshotFormats.JSON_V23, dao.snapshot?.payloadFormat)
            assertFalse(requireNotNull(dao.snapshot).payload.contains("itemCompletionGetProofs"))
            val restored = requireNotNull(repository.load())
            assertEquals(original.itemCompletionLedger, restored.itemCompletionLedger)
            assertTrue(restored.itemCompletionGetProofs.isEmpty())
            assertEquals(0L, restored.itemCompletionEvidenceGeneration)
            assertEquals(restored.itemCompletionLedger, repository.load()?.itemCompletionLedger)
        }
    }

    @Test fun v22UpgradeRetainsExactIndependentProgressAndAddsOnlyEmptyCompletionLedger() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        val state = completionTestState(false).copy(itemCompletionLedger = ItemCompletionLedger(),
            itemProgressLedger = progressTestLedger().copy(pending = listOf(progressTestMutation(true))))
        repository.save(state)
        val current = requireNotNull(dao.snapshot)
        dao.snapshot = current.copy(payloadFormat = PlannerSnapshotFormats.JSON_V22,
            payload = JsonObject(Json.parseToJsonElement(current.payload).jsonObject - "itemCompletionLedger").toString())
        val restored = requireNotNull(repository.load())
        assertEquals(state.itemProgressLedger, restored.itemProgressLedger)
        assertEquals(ItemCompletionLedger(), restored.itemCompletionLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V23, dao.snapshot?.payloadFormat)
    }

    @Test fun legacyInjectionRuntimeProofAndMissingClosedLedgerFieldsFailWithoutRewrite() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(completionTestState(false))
        val original = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(original.payload).jsonObject
        val variants = listOf(original.copy(payloadFormat = PlannerSnapshotFormats.JSON_V22),
            original.copy(payload = JsonObject(root - "itemCompletionLedger").toString()),
            original.copy(payload = JsonObject(root + ("itemCompletionGetProofs" to JsonObject(emptyMap()))).toString()),
            original.copy(payload = JsonObject(root + ("itemCompletionLedger" to JsonObject(root.getValue("itemCompletionLedger").jsonObject - "needsCanonicalCatchUp"))).toString()))
        variants.forEach { snapshot ->
            dao.snapshot = snapshot
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(snapshot, dao.snapshot)
        }
    }

    @Test fun pendingCompletionPinsBodylessTombstoneAndFailClosedSensitivity() = runBlocking {
        val dao = RawDao(); val repository = RoomPlannerStateRepository(dao) { 1_000 }
        val pending = completionTestMutation(true).copy(wasSensitive = true)
        val state = completionTestState(false).copy(canonicalItems = emptyList(), itemCompletionLedger = completionTestLedger().copy(pending = listOf(pending)),
            canonicalRecentlyDeleted = listOf(CanonicalRecentlyDeletedRecord(id = PROGRESS_ITEM, revision = 9,
                deletedAt = PROGRESS_NOW, parentId = null, lastKnownItem = null)))
        repository.save(state)
        assertEquals(listOf(pending), repository.load()?.itemCompletionLedger?.pending)
        assertEquals(PROGRESS_ITEM, repository.load()?.canonicalRecentlyDeleted?.single()?.id)
    }

    private class RawDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int) = snapshot
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }
}
