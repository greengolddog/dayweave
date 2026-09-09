package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.ITEM_PROGRESS_JSON
import com.greengolddog.dayweave.model.ItemProgressComponent
import com.greengolddog.dayweave.model.ItemProgressDisposition
import com.greengolddog.dayweave.model.ItemProgressLedger
import com.greengolddog.dayweave.model.ItemProgressObservation
import com.greengolddog.dayweave.model.ItemProgressRequest
import com.greengolddog.dayweave.model.ItemProgressSnapshot
import com.greengolddog.dayweave.model.ItemProgressValue
import com.greengolddog.dayweave.model.PendingItemProgressMutation
import com.greengolddog.dayweave.model.requireStrictItemProgressJson
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Test

class ItemProgressPersistenceTest {
    @Test
    fun exactSubmittedBytesPrivacyDispositionAndHistoricalObservationSurviveRestart() = runBlocking {
        for (disposition in ItemProgressDisposition.entries) {
            val dao = RawDao()
            val repository = RoomPlannerStateRepository(dao) { 1_000 }
            val original = state(disposition)
            repository.save(original)
            assertEquals(PlannerSnapshotFormats.JSON_V23, dao.snapshot?.payloadFormat)
            val first = requireNotNull(repository.load())
            assertEquals(original.itemProgressLedger, first.itemProgressLedger)
            assertEquals(original.itemProgressLedger.pending.single().requestJson,
                first.itemProgressLedger.pending.single().requestJson)
            // No cached canonical item is necessary to retain unresolved operation custody.
            assertEquals(emptyList<Any>(), first.canonicalItems)
            assertEquals(first.itemProgressLedger, repository.load()?.itemProgressLedger)
        }
    }

    @Test
    fun v21AddsOnlyEmptyProgressStateAndDoesNotReinterpretCurrentStructuralMarkers() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(DayWeaveUiState(pendingCanonicalAuthoringMutations = listOf(
            PendingCanonicalAuthoringMutation(id = OPERATION, itemId = ITEM,
                operation = CanonicalAuthoringOperation.CREATE,
                draft = CanonicalItemDraft(title = "Synthetic retained draft", timezoneName = "UTC"),
                createdAt = INSTANT),
        )))
        val old = requireNotNull(dao.snapshot)
        val oldRoot = Json.parseToJsonElement(old.payload).jsonObject - "itemProgressLedger" - "itemCompletionLedger"
        dao.snapshot = old.copy(payload = JsonObject(oldRoot).toString(), payloadFormat = PlannerSnapshotFormats.JSON_V21)
        assertEquals(ItemProgressLedger(), repository.load()?.itemProgressLedger)
        assertEquals(PlannerSnapshotFormats.JSON_V23, dao.snapshot?.payloadFormat)
        val newRoot = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        assertEquals(oldRoot, newRoot - "itemProgressLedger" - "itemCompletionLedger")
    }

    @Test
    fun everyPredecessorRejectsInjectedProgressEvenEmptyOrNullBeforeRewriting() = runBlocking {
        val formats = (1..21).map { version ->
            PlannerSnapshotFormats::class.java.getDeclaredField("JSON_V$version").get(null) as String
        }
        for (format in formats) {
            for (ledger in listOf(JsonNull, ITEM_PROGRESS_JSON.encodeToJsonElement(ItemProgressLedger()),
                    ITEM_PROGRESS_JSON.encodeToJsonElement(state().itemProgressLedger))) {
                val dao = RawDao()
                val repository = RoomPlannerStateRepository(dao) { 1_000 }
                repository.save(DayWeaveUiState())
                val current = requireNotNull(dao.snapshot)
                val root = Json.parseToJsonElement(current.payload).jsonObject
                val injected = current.copy(payload = JsonObject(root + ("itemProgressLedger" to ledger)).toString(),
                    payloadFormat = format)
                dao.snapshot = injected
                assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
                assertEquals(injected, dao.snapshot)
            }
        }
    }

    @Test
    fun v22RequiresLedgerAndEveryExplicitLedgerField() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(state())
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val ledger = root.getValue("itemProgressLedger").jsonObject
        val malformed = listOf(JsonObject(root - "itemProgressLedger"),
            JsonObject(root + ("itemProgressLedger" to JsonNull))) + ledger.keys.map { field ->
            JsonObject(root + ("itemProgressLedger" to JsonObject(ledger - field)))
        }
        for (payload in malformed) {
            dao.snapshot = stored.copy(payload = payload.toString())
            val before = dao.snapshot
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(before, dao.snapshot)
        }
    }

    @Test
    fun v22RejectsMissingJournalFieldsIncludingStickyPrivacyAndExactIdentity() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(state())
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val ledger = root.getValue("itemProgressLedger").jsonObject
        val journal = (ledger.getValue("pending") as JsonArray).single().jsonObject
        for (field in journal.keys) {
            val modified = JsonObject(ledger + ("pending" to JsonArray(listOf(JsonObject(journal - field)))))
            dao.snapshot = stored.copy(payload = JsonObject(root + ("itemProgressLedger" to modified)).toString())
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
        }
    }

    @Test
    fun v22RejectsMissingObservationProofAndSnapshotFields() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(state())
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val ledger = root.getValue("itemProgressLedger").jsonObject
        val observation = ledger.getValue("observations").jsonObject.getValue(ITEM).jsonObject
        val snapshot = observation.getValue("snapshot").jsonObject
        val variants = observation.keys.map { JsonObject(observation - it) } + snapshot.keys.map {
            JsonObject(observation + ("snapshot" to JsonObject(snapshot - it)))
        }
        for (variant in variants) {
            val modified = JsonObject(ledger + ("observations" to JsonObject(mapOf(ITEM to variant))))
            dao.snapshot = stored.copy(payload = JsonObject(root + ("itemProgressLedger" to modified)).toString())
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
        }
    }

    @Test
    fun enclosingCredentialMismatchIsRejectedOnSaveAndLoadEvenForEmptyBoundLedger() = runBlocking {
        for (ledger in listOf(state().itemProgressLedger, ItemProgressLedger(syncOrigin = ORIGIN, configurationId = CONFIG))) {
            val dao = RawDao()
            val repository = RoomPlannerStateRepository(dao) { 1_000 }
            assertThrows(SerializationException::class.java) {
                runBlocking { repository.save(DayWeaveUiState(itemProgressLedger = ledger)) }
            }
            repository.save(state().copy(itemProgressLedger = ledger))
            val stored = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(stored.payload).jsonObject
            for (field in listOf("canonicalSyncOrigin", "canonicalConfigurationId")) {
                dao.snapshot = stored.copy(payload = JsonObject(root + (field to JsonPrimitive("wrong-binding"))).toString())
                assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            }
        }
    }

    @Test
    fun v22RejectsRawDuplicateAndEscapedEquivalentKeysBeforeTreeDecoding() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(state())
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val empty = ITEM_PROGRESS_JSON.encodeToString(ItemProgressLedger())
        val variants = listOf(
            "{\"itemProgressLedger\":$empty," + stored.payload.drop(1),
            stored.payload.replace("\"wasSensitive\":true", "\"wasSensitive\":false,\"wasSensitive\":true"),
            stored.payload.replace("\"wasSensitive\":true", "\"wasSensitive\":false,\"was\\u0053ensitive\":true"),
            "{\"itemProgressLedge\\u0072\":${root.getValue("itemProgressLedger")}," + stored.payload.drop(1),
        )
        for (payload in variants) {
            require(payload != stored.payload)
            dao.snapshot = stored.copy(payload = payload)
            val before = dao.snapshot
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
            assertEquals(before, dao.snapshot)
        }
    }

    @Test
    fun envelopeDuplicateFenceDoesNotNarrowUnrelatedLegacyNumberGrammar() {
        val unrelatedNumbers = "{\"legacy\":[1.5,-0.0,1e-3],\"itemProgressLedger\":{\"schemaVersion\":1}}"
        requireStrictItemProgressJson(unrelatedNumbers, integersOnly = false, maxDepth = 128)
        assertThrows(IllegalArgumentException::class.java) {
            requireStrictItemProgressJson(unrelatedNumbers)
        }
    }

    @Test
    fun unknownLedgerShapeAndAlteredFrozenRequestCannotBeAdmitted() = runBlocking {
        val dao = RawDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(state())
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val ledger = root.getValue("itemProgressLedger").jsonObject
        val journal = (ledger.getValue("pending") as JsonArray).single().jsonObject
        val variants = listOf(
            JsonObject(ledger + ("futureAuthority" to JsonPrimitive(true))),
            JsonObject(ledger + ("schemaVersion" to JsonPrimitive(2))),
            JsonObject(ledger + ("pending" to JsonArray(listOf(JsonObject(journal +
                ("expectedProgressRevision" to JsonPrimitive(99))))))),
        )
        for (variant in variants) {
            dao.snapshot = stored.copy(payload = JsonObject(root + ("itemProgressLedger" to variant)).toString())
            assertThrows(SerializationException::class.java) { runBlocking { repository.load() } }
        }
        assertNotNull(dao.snapshot)
    }

    private fun state(disposition: ItemProgressDisposition = ItemProgressDisposition.PENDING): DayWeaveUiState {
        val components = listOf(ItemProgressComponent(COMPONENT, "Synthetic quantity",
            ItemProgressValue.Quantity("999999999999.999999", "chapters", null)))
        val request = ItemProgressRequest(operationId = OPERATION, expectedItemRevision = 7,
            expectedProgressRevision = 2, components = components)
        val journal = PendingItemProgressMutation(operationId = OPERATION, itemId = ITEM,
            syncOrigin = ORIGIN, configurationId = CONFIG, expectedItemRevision = 7, expectedProgressRevision = 2,
            requestJson = " \n" + ITEM_PROGRESS_JSON.encodeToString(request) + "\n ",
            createdAt = INSTANT, submittedAt = INSTANT, disposition = disposition, wasSensitive = true)
        return DayWeaveUiState(canonicalSyncOrigin = ORIGIN, canonicalConfigurationId = CONFIG,
            itemProgressLedger = ItemProgressLedger(syncOrigin = ORIGIN, configurationId = CONFIG,
                observations = mapOf(ITEM to ItemProgressObservation(ItemProgressSnapshot(1, ITEM, 7, 2,
                    components, INSTANT), INSTANT, true)), pending = listOf(journal)))
    }

    /** Deliberately raw: injection tests must never pass through legacy fixture normalization. */
    private class RawDao : PlannerSnapshotDao {
        var snapshot: PlannerSnapshotEntity? = null
        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot
        override suspend fun save(snapshot: PlannerSnapshotEntity) { this.snapshot = snapshot }
    }

    private companion object {
        const val ORIGIN = "https://api.example.test/"
        const val CONFIG = "synthetic-progress-binding"
        const val ITEM = "11111111-1111-4111-8111-111111111111"
        const val COMPONENT = "22222222-2222-4222-8222-222222222222"
        const val OPERATION = "33333333-3333-4333-8333-333333333333"
        const val INSTANT = "2026-09-08T10:00:00Z"
    }
}
