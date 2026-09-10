package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.net.Proxy
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.time.Instant
import java.time.ZoneId
import java.time.temporal.ChronoUnit
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/** Real HTTP and the production encrypted-snapshot codec; no Keystore/SQLCipher device claim. */
class NativeCompletionConvergenceTest {
    @Test fun runSelectedPhaseAgainstDisposableService() = runBlocking {
        val rawPath = System.getenv("DAYWEAVE_NATIVE_COMPLETION_CONFIG")
        val rawPhase = System.getenv("DAYWEAVE_NATIVE_COMPLETION_PHASE")
        assumeTrue("Requires explicit private disposable-service config and phase", nativeConvergenceOptIn(rawPath, rawPhase))
        val phase = requireNotNull(rawPhase).also { require(it in NATIVE_COMPLETION_PHASES) }
        val config = try { NativeCompletionConfig.read(Path.of(requireNotNull(rawPath))) }
        catch (_: Exception) { throw AssertionError("Invalid private disposable completion configuration") }
        val directory = config.workDirectory.resolve("android")
        val disk = NativeConvergenceDisk(directory, config.binding, prepare = phase == "prepare_offline")
        require(!Files.exists(directory.resolve("$phase.json"), NOFOLLOW_LINKS)) { "Completion phase already passed" }
        val repository = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(disk))
        if (phase == "prepare_offline") repository.save(DayWeaveUiState(canonicalSyncOrigin = config.baseUrl,
            canonicalConfigurationId = config.configurationId))
        else require(disk.read("prepared_root_journal") != null && disk.read("snapshot") != null)
        val credentials = NativeCompletionCredentials(config, phase)
        val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder().proxy(Proxy.NO_PROXY)
            .followRedirects(false).followSslRedirects(false).callTimeout(20, TimeUnit.SECONDS).build()
        val completion = ObservedCompletionTransport(OkHttpItemCompletionTransport(client))
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(DayWeaveUiState(), repository, scope)
            withTimeout(20_000) { store.loadState.first { it != PlannerLoadState.LOADING } }
            assertEquals(PlannerLoadState.READY, store.loadState.value)
            assertEquals(config.baseUrl, store.state.value.canonicalSyncOrigin)
            assertEquals(config.configurationId, store.state.value.canonicalConfigurationId)
            assertTrue("Restart cannot restore GET permission", store.state.value.itemCompletionGetProofs.isEmpty())
            val canonical = CanonicalSyncManager(store, credentials, OkHttpCanonicalPlannerTransport(client),
                // Always exercise a sub-microsecond clock against the real
                // preview/publication API, without moving ahead of wall time.
                now = { Instant.now().truncatedTo(ChronoUnit.MICROS).minusNanos(1) },
                zoneId = { ZoneId.of("UTC") }, completionTransport = completion)
            val manager = ItemCompletionSyncManager(store, credentials, completion)
            val flow = CompletionFlow(config, directory, disk, repository, store, manager, canonical, completion)
            completion.beforePut = { item, body ->
                val saved = requireNotNull(repository.load()).itemCompletionLedger.pending.single { it.itemId == item }
                assertNotNull(saved.submittedAt)
                assertEquals(body, saved.requestJson)
            }
            when (phase) {
                "prepare_offline" -> flow.prepareOffline()
                "conflict_keep_open" -> flow.conflictKeepOpen()
                "catchup_automatic_optional" -> flow.catchUpAutomaticOptional()
                "verify_cascade_and_child" -> flow.verifyCascadeAndChild(credentials)
            }
            val durable = requireNotNull(repository.load())
            flow.assertGraph(durable, if (phase == "verify_cascade_and_child") config.finalIds else config.initialIds)
            assertEquals(store.state.value.canonicalItems, durable.canonicalItems)
            assertEquals(store.state.value.itemCompletionLedger, durable.itemCompletionLedger)
            assertTrue(durable.itemCompletionGetProofs.isEmpty())
            assertTrue(durable.pendingCanonicalAuthoringMutations.isEmpty())
            assertNull(durable.pendingSchedulePublication)
            assertNull(durable.canonicalExecutionSession)
            assertEquals(0L, durable.canonicalExecutionRevision)
            assertEquals(ItemProgressLedger(), durable.itemProgressLedger)
            val rootObservation = durable.itemCompletionLedger.observations[config.rootId]?.snapshot
            writeNativeCompletionMarker(directory.resolve("$phase.json"), buildJsonObject {
                put("schema_version", 1); put("run_id", config.runId); put("phase", phase); put("status", "passed")
                put("pending_count", durable.itemCompletionLedger.pending.size)
                put("needs_canonical_catch_up", durable.itemCompletionLedger.needsCanonicalCatchUp)
                put("items", nativeCompletionItems(durable.canonicalItems))
                put("root_completion", rootObservation?.let { ITEM_PROGRESS_JSON.encodeToJsonElement(it) } ?: JsonNull)
            })
        } finally {
            scope.cancel()
            client.dispatcher.executorService.shutdown()
            client.connectionPool.evictAll()
        }
    }
}

private class CompletionFlow(
    private val config: NativeCompletionConfig, private val directory: Path, private val disk: NativeConvergenceDisk,
    private val repository: RoomPlannerStateRepository, private val store: PlannerStore,
    private val manager: ItemCompletionSyncManager, private val canonical: CanonicalSyncManager,
    private val transport: ObservedCompletionTransport,
) {
    suspend fun prepareOffline() {
        catchUp()
        assertGraph(store.state.value, config.initialIds)
        assertRootBlocked()
        assertEquals("planned", item(config.branchId).status)
        assertEquals("planned", item(config.requiredLeafId).status)
        assertEquals("planned", item(config.optionalLeafId).status)
        val reviewed = fresh(config.rootId)
        assertEquals(0L, reviewed.state.revision)
        assertEquals(ItemCompletionMode.AUTOMATIC, reviewed.state.mode)
        assertEquals(ItemCompletionCounts(3, 0, 3, 0), reviewed.counts)
        val queued = queue("b", config.rootId, reviewed, true, ItemCompletionMode.KEEP_OPEN)
        assertNull(queued.submittedAt)
        transport.dropBeforeRealPut = true
        assertFalse(manager.replay())
        val saved = durable().itemCompletionLedger.pending.single()
        assertEquals(queued.operationId, saved.operationId)
        assertEquals(queued.requestJson, saved.requestJson)
        assertNotNull(saved.submittedAt)
        assertEquals(ItemCompletionDisposition.PENDING, saved.disposition)
        assertEquals(0, transport.actualPuts)
        assertFalse(durable().itemCompletionLedger.needsCanonicalCatchUp)
        disk.write("prepared_root_journal", ITEM_PROGRESS_JSON.encodeToString(saved))
        disk.write("prepared_canonical", nativeCompletionItems(store.state.value.canonicalItems).toString())
        assertRootBlocked()
    }

    suspend fun conflictKeepOpen() {
        val original = decodeExactItemCompletion<PendingItemCompletionMutation>(requireNotNull(disk.read("prepared_root_journal")))
        assertEquals(original, durable().itemCompletionLedger.pending.single())
        assertEquals(requireNotNull(disk.read("prepared_canonical")), nativeCompletionItems(store.state.value.canonicalItems).toString())
        assertTrue(manager.replay())
        assertEquals(listOf(original.requestJson), transport.putBodies)
        assertEquals(listOf(ItemCompletionFailureCode.ITEM_STALE), transport.failures)
        val rejected = durable().itemCompletionLedger.pending.single()
        assertEquals(original.copy(disposition = ItemCompletionDisposition.REVIEW_REQUIRED), rejected)
        assertTrue(durable().itemCompletionLedger.needsCanonicalCatchUp)
        catchUp()
        val manual = fresh(config.rootId)
        assertEquals(1L, manual.state.revision)
        assertEquals(ItemCompletionMode.COMPLETE, manual.state.mode)
        assertRootCompleted(manual, ItemCompletionProvenanceKind.MANUAL)
        assertEquals(rejected, durable().itemCompletionLedger.pending.single())
        disk.write("manual_completed_canonical", nativeCompletionItems(store.state.value.canonicalItems).toString())
        val replacement = queue("c", config.rootId, manual, true, ItemCompletionMode.KEEP_OPEN, rejected.operationId)
        assertNotEquals(original.operationId, replacement.operationId)
        assertNotEquals(original.requestJson, replacement.requestJson)
        assertTrue(manager.replay())
        assertTrue(durable().itemCompletionLedger.pending.isEmpty())
        assertTrue(durable().itemCompletionLedger.needsCanonicalCatchUp)
        assertEquals(2L, durable().itemCompletionLedger.observations.getValue(config.rootId).snapshot.state.revision)
        assertEquals(ItemCompletionMode.KEEP_OPEN, durable().itemCompletionLedger.observations.getValue(config.rootId).snapshot.state.mode)
        assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
        // Deny only the subsequent canonical callback: receipt custody already committed.
        assertFalse(canonical.refreshItemProgressCanonicalEvidence { false })
        assertEquals(requireNotNull(disk.read("manual_completed_canonical")), nativeCompletionItems(durable().canonicalItems).toString())
        assertEquals("completed", item(config.rootId).status)
        assertTrue(store.hasCredentialReplacementBlocker())
    }

    suspend fun catchUpAutomaticOptional() {
        assertTrue(durable().itemCompletionLedger.pending.isEmpty())
        assertTrue(durable().itemCompletionLedger.needsCanonicalCatchUp)
        assertEquals(requireNotNull(disk.read("manual_completed_canonical")), nativeCompletionItems(store.state.value.canonicalItems).toString())
        assertFalse(manager.load(config.rootId))
        assertTrue(transport.getItems.isEmpty())
        catchUp()
        assertRootBlocked()
        val keepOpen = fresh(config.rootId)
        assertEquals(2L, keepOpen.state.revision)
        assertEquals(ItemCompletionMode.KEEP_OPEN, keepOpen.state.mode)
        queue("d", config.rootId, keepOpen, true, ItemCompletionMode.AUTOMATIC)
        assertTrue(manager.replay())
        assertTrue(durable().itemCompletionLedger.needsCanonicalCatchUp)
        catchUp()
        assertRootBlocked()
        val optional = fresh(config.optionalLeafId)
        assertTrue(optional.state.requiredForParent)
        queue("e", config.optionalLeafId, optional, false, optional.state.mode)
        assertTrue(manager.replay())
        catchUp()
        val root = fresh(config.rootId)
        assertEquals(3L, root.state.revision)
        assertEquals(ItemCompletionMode.AUTOMATIC, root.state.mode)
        assertEquals(ItemCompletionCounts(2, 0, 2, 0), root.counts)
        assertRootBlocked()
        assertEquals("planned", item(config.optionalLeafId).status)
        assertEquals("planned", item(config.requiredLeafId).status)
        assertFalse(durable().itemCompletionLedger.observations.getValue(config.optionalLeafId).snapshot.state.requiredForParent)
    }

    suspend fun verifyCascadeAndChild(credentials: NativeCompletionCredentials) {
        assertTrue(durable().itemCompletionLedger.pending.isEmpty())
        assertFalse(durable().itemCompletionLedger.needsCanonicalCatchUp)
        catchUp()
        val root = fresh(config.rootId)
        assertRootCompleted(root, ItemCompletionProvenanceKind.AUTOMATIC)
        assertEquals(4L, root.state.revision)
        assertEquals(ItemCompletionCounts(2, 2, 0, 0), root.counts)
        assertEquals("completed", item(config.branchId).status)
        val required = item(config.requiredLeafId)
        val optional = item(config.optionalLeafId)
        assertEquals("completed", required.status)
        assertEquals("planned", optional.status)
        val branch = fresh(config.branchId)
        assertEquals(ItemCompletionProvenanceKind.AUTOMATIC, branch.state.provenance?.kind)
        assertTrue(store.state.value.hasQualifiedCompletedParent(config.branchId, config.newChildId, store.state.value.completionLocalEvidence()))
        val queued = requireNotNull(store.enqueueCanonicalCreate(CanonicalItemDraft(title = "Synthetic new required child",
            timezoneName = "UTC", placement = CanonicalDraftPlacement.PLANNED, durationSeconds = 60,
            parentId = config.branchId), config.newChildId))
        assertTrue(queued.persistence.awaitDurable())
        assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
        var creates = 0
        credentials.beforeCreate = { key, body ->
            creates++
            val saved = durable().pendingCanonicalAuthoringMutations.single()
            assertTrue(saved.isSubmitted)
            assertEquals(saved.idempotencyKey, key)
            assertEquals(config.newChildId, saved.itemId)
            assertEquals(config.branchId, saved.draft?.parentId)
            assertEquals(2, transport.getItems.count { it == config.branchId })
            writeNativeCompletionPrivateBytes(directory.resolve("new-child-request.json"), body)
        }
        val outcome = canonical.refreshAndCompose()
        assertEquals("Canonical create/publication outcome=$outcome; phase=${canonical.state.value.phase}",
            CanonicalRefreshOutcome.SUCCESS, outcome)
        assertEquals(1, creates)
        assertTrue(durable().pendingCanonicalAuthoringMutations.isEmpty())
        assertEquals(0, Instant.parse(requireNotNull(durable().scheduleGeneratedAt)).nano % 1_000)
        catchUp()
        val reopened = fresh(config.rootId)
        assertRootBlocked()
        assertEquals(5L, reopened.state.revision)
        assertEquals(ItemCompletionMode.AUTOMATIC, reopened.state.mode)
        assertNull(reopened.state.provenance)
        assertEquals(ItemCompletionCounts(3, 1, 2, 0), reopened.counts)
        assertEquals("planned", item(config.branchId).status)
        assertEquals("planned", item(config.newChildId).status)
        assertEquals(config.branchId, item(config.newChildId).parentId)
        assertEquals(required, item(config.requiredLeafId))
        assertEquals(optional, item(config.optionalLeafId))
        assertGraph(store.state.value, config.finalIds)
    }

    private suspend fun queue(letter: String, itemId: String, reviewed: ItemCompletionSnapshot, required: Boolean,
        mode: ItemCompletionMode, replacing: String? = null,
    ): PendingItemCompletionMutation {
        assertTrue(manager.stage(itemId, reviewed, required, mode, replacing))
        val saved = durable().itemCompletionLedger.pending.single()
        assertTrue(saved.wasSensitive)
        assertNull(saved.submittedAt)
        assertEquals(reviewed.evidenceHash, saved.request().expectedEvidenceHash)
        writeNativeCompletionPrivateBytes(directory.resolve("operation-$letter.json"), saved.requestJson)
        return saved
    }

    private suspend fun catchUp() {
        assertTrue(canonical.refreshItemProgressCanonicalEvidence { true })
        assertFalse(durable().itemCompletionLedger.needsCanonicalCatchUp)
        assertFalse(store.state.value.canonicalDeltaCursor.isNullOrBlank())
    }
    private suspend fun fresh(itemId: String): ItemCompletionSnapshot {
        assertTrue(manager.load(itemId))
        return requireNotNull(store.state.value.currentCompletionProof(itemId)).snapshot.also {
            assertEquals(item(itemId).revision, it.itemRevision)
        }
    }
    private suspend fun durable() = requireNotNull(repository.load())
    private fun item(id: String) = requireNotNull(store.state.value.completionItem(id))
    private fun assertRootBlocked() {
        val root = item(config.rootId)
        assertEquals("blocked", root.status)
        assertEquals(CanonicalBlockedReasonKind.MANUAL, root.blockedReasonKind)
        assertNull(root.blockedByItemId)
        assertEquals(COMPLETION_WAIT_REASON, root.blockedReason)
    }
    private fun assertRootCompleted(snapshot: ItemCompletionSnapshot, kind: ItemCompletionProvenanceKind) {
        val root = item(config.rootId)
        assertEquals("completed", root.status)
        assertNull(root.blockedReasonKind); assertNull(root.blockedByItemId); assertNull(root.blockedReason)
        assertEquals(kind, snapshot.state.provenance?.kind)
        assertEquals(ItemCompletionReopening("blocked", "manual", null, COMPLETION_WAIT_REASON), snapshot.state.provenance?.reopen)
    }
    fun assertGraph(state: DayWeaveUiState, expected: Set<String>) {
        assertEquals(expected, state.canonicalItems.map { it.id }.toSet())
        assertEquals(expected.size, state.canonicalItems.size)
        assertTrue(state.canonicalItems.all { it.deletedAt == null })
        assertTrue(state.canonicalRecentlyDeleted.isEmpty())
        val items = state.canonicalItems.associateBy { it.id }
        assertNull(items.getValue(config.rootId).parentId)
        assertEquals(config.rootId, items.getValue(config.branchId).parentId)
        assertEquals(config.branchId, items.getValue(config.requiredLeafId).parentId)
        assertEquals(config.rootId, items.getValue(config.optionalLeafId).parentId)
        assertEquals("goal", items.getValue(config.rootId).kind)
        assertEquals("project", items.getValue(config.branchId).kind)
        assertTrue(state.canonicalItems.all { state.completionItem(it.id) != null })
    }
}

private class ObservedCompletionTransport(private val real: ItemCompletionTransport) : ItemCompletionTransport {
    val getItems = mutableListOf<String>()
    val putBodies = mutableListOf<String>()
    val failures = mutableListOf<ItemCompletionFailureCode>()
    var actualPuts = 0
    var dropBeforeRealPut = false
    var beforePut: suspend (String, String) -> Unit = { _, _ -> }
    override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String): ItemCompletionSnapshot {
        getItems += itemId
        return real.get(configuration, itemId)
    }
    override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemCompletionMutationResult {
        beforePut(itemId, requestJson)
        putBodies += requestJson
        if (dropBeforeRealPut) throw IOException("Synthetic failure before disposable PUT")
        actualPuts++
        return try { real.put(configuration, itemId, requestJson) }
        catch (error: ItemCompletionApiException.Definitive) { failures += error.code; throw error }
    }
}
