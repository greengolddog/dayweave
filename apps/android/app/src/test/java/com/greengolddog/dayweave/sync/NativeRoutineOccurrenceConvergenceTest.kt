package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.net.Proxy
import java.net.URI
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.util.UUID
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okio.Buffer
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/** Opt-in real loopback HTTP and production encrypted Room snapshot codec; no device/Keystore claim. */
class NativeRoutineOccurrenceConvergenceTest {
    @Test fun runSelectedPhaseAgainstDisposableService(): Unit = runBlocking {
        val rawPath = System.getenv("DAYWEAVE_NATIVE_ROUTINE_CONFIG")
        val rawPhase = System.getenv("DAYWEAVE_NATIVE_ROUTINE_PHASE")
        assumeTrue("Requires explicit private disposable-service config and phase", nativeConvergenceOptIn(rawPath, rawPhase))
        val phase = requireNotNull(rawPhase).also { require(it in ROUTINE_PHASES) }
        val config = try { NativeRoutineConfig.read(Path.of(requireNotNull(rawPath))) }
        catch (_: Exception) { throw AssertionError("Invalid private disposable routine configuration") }
        val directory = config.workDirectory.resolve("android")
        val disk = NativeConvergenceDisk(directory, config.binding, prepare = phase == "prepare_offline")
        require(!Files.exists(directory.resolve("$phase.json"), NOFOLLOW_LINKS)) { "Routine phase already passed" }
        val repository = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(disk))
        if (phase == "prepare_offline") repository.save(DayWeaveUiState(canonicalSyncOrigin = config.baseUrl,
            canonicalConfigurationId = config.configurationId))
        else require(disk.read("prepared_b_journal") != null && disk.read("snapshot") != null)
        val credentials = NativeRoutineCredentials(config, phase)
        val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder().proxy(Proxy.NO_PROXY)
            .followRedirects(false).followSslRedirects(false).callTimeout(20, TimeUnit.SECONDS).build()
        val observed = ObservedRoutineTransport(OkHttpRoutineOccurrenceTransport(client))
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val store = PlannerStore(DayWeaveUiState(), repository, scope, nowEpochMillis = { config.asOf.toEpochMilli() })
            withTimeout(20_000) { store.loadState.first { it != PlannerLoadState.LOADING } }
            assertEquals(PlannerLoadState.READY, store.loadState.value)
            assertEquals(config.baseUrl, store.state.value.canonicalSyncOrigin)
            assertEquals(config.configurationId, store.state.value.canonicalConfigurationId)
            assertNull("Restart must not restore remote Defer admission", store.state.value.routineOccurrenceDeferAdmission)
            assertEquals(0L, store.state.value.routineOccurrenceAuthorityGeneration)
            assertTrue(store.state.value.itemCompletionGetProofs.isEmpty())
            val canonical = CanonicalSyncManager(store, credentials, OkHttpCanonicalPlannerTransport(client),
                now = { config.asOf }, zoneId = { ZoneId.of("UTC") }, localScheduleComposer = null)
            val manager = RoutineOccurrenceSyncManager(store, credentials, observed, now = { config.asOf })
            assertNull("Restart must not restore occurrence GET permission", manager.state.value.reviewed)
            val flow = NativeRoutineFlow(config, directory, disk, repository, store, manager, canonical, observed, credentials)
            observed.beforePut = { instanceId, memberId, body ->
                val saved = requireNotNull(repository.load()).routineOccurrenceLedger.pending.single { it.instanceId == instanceId }
                assertEquals(config.instanceId, instanceId)
                assertEquals(saved.memberId, memberId)
                assertNotNull("Production submitted marker must precede the HTTP boundary", saved.submittedAt)
                assertEquals(RoutineOccurrenceDisposition.PENDING, saved.disposition)
                assertEquals(saved.requestJson, body)
                assertEquals(saved.request, decodeExactRoutineOccurrence<RoutineOccurrenceRequest>(body))
                val files = listOf("b", "c", "d", "e", "f").map { directory.resolve("operation-$it.json") }
                    .filter { Files.exists(it, NOFOLLOW_LINKS) }
                val frozen = files.map { file ->
                    requirePrivateConvergencePath(file)
                    require(Files.size(file) in 1..MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES.toLong())
                    val bytes = Files.readAllBytes(file)
                    require(bytes.size in 1..MAX_ROUTINE_OCCURRENCE_REQUEST_BYTES)
                    String(bytes, Charsets.UTF_8)
                }.single { decodeExactRoutineOccurrence<RoutineOccurrenceRequest>(it).operationId == saved.operationId }
                assertEquals("Every first send and restart retry uses the original saved bytes", frozen, body)
            }
            credentials.beforePublication = { body ->
                val saved = requireNotNull(repository.load()).pendingSchedulePublication
                assertNotNull("Publication journal must be durable before HTTP", saved)
                assertEquals(requireNotNull(saved).request.bodyJson, body)
            }
            when (phase) {
                "prepare_offline" -> flow.prepareOffline()
                "conflict_keep_open" -> flow.conflictKeepOpen()
                "finish" -> flow.finish()
                "verify" -> flow.verify()
            }
            val durable = requireNotNull(repository.load())
            flow.assertCommon(durable)
            assertEquals(store.state.value.canonicalItems, durable.canonicalItems)
            assertEquals(store.state.value.routineOccurrenceLedger, durable.routineOccurrenceLedger)
            assertNull(durable.routineOccurrenceDeferAdmission)
            assertEquals(0L, durable.routineOccurrenceAuthorityGeneration)
            assertTrue(durable.itemCompletionGetProofs.isEmpty())
            assertNull(durable.pendingSchedulePublication)
            val ledger = durable.routineOccurrenceLedger
            writeNativeCompletionMarker(directory.resolve("$phase.json"), buildJsonObject {
                put("schema_version", 1); put("run_id", config.runId); put("phase", phase); put("status", "passed")
                put("pending_count", ledger.pending.size)
                put("submitted_count", ledger.pending.count { it.submittedAt != null })
                put("receipt_target_count", ledger.minimumCatchUpRevisions.size)
                put("needs_remote_schedule_catch_up", ledger.needsRemoteScheduleCatchUp)
                put("has_pending_publication", durable.pendingSchedulePublication != null)
                put("terminal_cursor", ledger.deltaCursor)
                put("items", nativeCompletionItems(durable.canonicalItems))
                put("occurrence", ITEM_PROGRESS_JSON.encodeToJsonElement(ledger.observations.getValue(config.instanceId).snapshot.aggregate))
                put("sentinel", ITEM_PROGRESS_JSON.encodeToJsonElement(ledger.observations.getValue(config.sentinelInstanceId).snapshot.aggregate))
                put("publication_operation_id", durable.pendingSchedulePublication?.idempotencyKey ?: credentials.publicationOperations.lastOrNull())
                put("publication_revision_id", durable.publishedScheduleProof?.revision?.id)
            })
        } finally {
            scope.cancel()
            client.dispatcher.executorService.shutdown()
            client.connectionPool.evictAll()
        }
    }
}

private class NativeRoutineFlow(
    private val config: NativeRoutineConfig, private val directory: Path, private val disk: NativeConvergenceDisk,
    private val repository: RoomPlannerStateRepository, private val store: PlannerStore,
    private val manager: RoutineOccurrenceSyncManager, private val canonical: CanonicalSyncManager,
    private val transport: ObservedRoutineTransport, private val credentials: NativeRoutineCredentials,
) {
    private val selected = RoutineOccurrenceSelection(config.rootId, config.occurrenceId)

    suspend fun prepareOffline() {
        assertEquals(CanonicalRefreshOutcome.SUCCESS, canonical.refreshAndCompose())
        catchUp()
        assertCommon(durable())
        val initial = fresh()
        assertInitial(initial.aggregate)
        val sentinel = occurrence(config.sentinelInstanceId)
        assertInitial(sentinel)
        disk.write("baseline_occurrence", ITEM_PROGRESS_JSON.encodeToString(initial.aggregate))
        disk.write("baseline_sentinel", ITEM_PROGRESS_JSON.encodeToString(sentinel))
        disk.write("prepared_canonical", ITEM_PROGRESS_JSON.encodeToString(store.state.value.canonicalItems.sortedBy { it.id }))
        val queued = queue("b", initial, config.requiredLeafId, RoutineOccurrenceAction.SetOutcome("skipped"))
        assertNull(queued.submittedAt)
        transport.failBeforeWire = true
        assertFalse(manager.replay())
        val saved = durable().routineOccurrenceLedger.pending.single()
        assertEquals(queued.copy(submittedAt = config.asOf.toString()), saved)
        assertNotNull(saved.submittedAt)
        assertEquals(0, transport.actualPuts)
        assertTrue(transport.failures.isEmpty())
        assertEquals(listOf(saved.requestJson), transport.putBodies)
        val beforeFailedPages = durable().routineOccurrenceLedger
        transport.failPages = true
        assertFalse(manager.refresh())
        assertEquals(beforeFailedPages, durable().routineOccurrenceLedger)
        assertTrue(beforeFailedPages.minimumCatchUpRevisions.isEmpty())
        assertFalse(beforeFailedPages.needsRemoteScheduleCatchUp)
        assertInitial(occurrence(config.instanceId))
        assertFalse(manager.discard(saved.operationId))
        disk.write("prepared_b_journal", ITEM_PROGRESS_JSON.encodeToString(saved))
    }

    suspend fun conflictKeepOpen() {
        assertTemplatesUnchanged()
        val original = decodeExactRoutineOccurrence<PendingRoutineOccurrenceMutation>(requireNotNull(disk.read("prepared_b_journal")))
        assertEquals(original, durable().routineOccurrenceLedger.pending.single())
        assertNull(manager.state.value.reviewed)
        assertTrue(manager.replay())
        assertEquals(0, transport.getInstances.size + transport.lookups.size)
        assertEquals(listOf(original.requestJson), transport.putBodies)
        assertEquals(listOf(RoutineOccurrenceFailureCode.INSTANCE_STALE), transport.failures)
        assertEquals(1, transport.actualPuts)
        val rejected = durable().routineOccurrenceLedger.pending.single()
        assertEquals(original.copy(disposition = RoutineOccurrenceDisposition.REVIEW_REQUIRED), rejected)
        assertEquals(1L, occurrence(config.instanceId).revision)
        assertTrue(durable().routineOccurrenceLedger.minimumCatchUpRevisions.isEmpty())
        assertTrue(manager.discard(original.operationId))
        assertTrue(durable().routineOccurrenceLedger.pending.isEmpty())
        assertTrue(manager.refresh())
        val awaitingFreshPublication = durable().routineOccurrenceLedger
        assertTrue(awaitingFreshPublication.needsRemoteScheduleCatchUp)
        assertNotNull(durable().publishedScheduleProof)
        assertFalse("Restored publication history cannot recreate a fresh remote-composition witness",
            manager.catchUpSchedule { null })
        assertEquals(awaitingFreshPublication, durable().routineOccurrenceLedger)
        catchUp()
        val reviewed = fresh()
        assertEquals(2L, reviewed.aggregate.revision)
        assertCompletedBranch(reviewed.aggregate)
        assertEquals("completed", member(reviewed.aggregate, config.rootId).status)
        queue("c", reviewed, config.rootId, RoutineOccurrenceAction.SetPolicy(true, ItemCompletionMode.KEEP_OPEN))
        assertTrue(manager.replay())
        assertReceiptTarget(3)
        catchUp()
        val current = fresh()
        assertEquals(3L, current.aggregate.revision)
        val root = member(current.aggregate, config.rootId)
        assertEquals(ItemCompletionMode.KEEP_OPEN, root.mode)
        assertEquals("planned", root.status)
        assertNull(root.completedAt)
        assertCompletedBranch(current.aggregate)
        assertEquals(ItemCompletionCounts(2, 2, 0, 0), current.members.single { it.itemId == config.rootId }.counts)
        assertRecovered()
    }

    suspend fun finish() {
        assertTemplatesUnchanged()
        catchUp()
        val keepOpen = fresh()
        assertEquals(3L, keepOpen.aggregate.revision)
        assertEquals(ItemCompletionMode.KEEP_OPEN, member(keepOpen.aggregate, config.rootId).mode)
        queue("d", keepOpen, config.rootId, RoutineOccurrenceAction.SetPolicy(true, ItemCompletionMode.AUTOMATIC))
        assertTrue(manager.replay())
        assertReceiptTarget(4)
        catchUp()
        val automatic = fresh()
        assertEquals(4L, automatic.aggregate.revision)
        val completedAt = requireNotNull(member(automatic.aggregate, config.rootId).completedAt)
        assertEquals(ItemCompletionMode.AUTOMATIC, member(automatic.aggregate, config.rootId).mode)
        val originalBlocked = member(automatic.aggregate, config.blockedLeafId).open
        assertEquals(ItemCompletionReopening("blocked", "manual", null, ROUTINE_WAIT_REASON), originalBlocked)
        queue("e", automatic, config.blockedLeafId, RoutineOccurrenceAction.SetOutcome("skipped"))
        assertTrue(manager.replay())
        assertReceiptTarget(5)
        catchUp()
        val skipped = fresh()
        assertEquals(5L, skipped.aggregate.revision)
        assertEquals("skipped", member(skipped.aggregate, config.blockedLeafId).status)
        assertEquals(originalBlocked, member(skipped.aggregate, config.blockedLeafId).open)
        assertEquals(completedAt, member(skipped.aggregate, config.rootId).completedAt)
        queue("f", skipped, config.blockedLeafId, RoutineOccurrenceAction.Reopen(originalBlocked))
        assertTrue(manager.replay())
        assertReceiptTarget(6)
        catchUp()
        assertFinal(fresh())
        assertEquals(completedAt, member(occurrence(config.instanceId), config.rootId).completedAt)
        assertRecovered()
    }

    suspend fun verify() {
        assertTemplatesUnchanged()
        assertNull(manager.state.value.reviewed)
        assertNull(store.state.value.routineOccurrenceDeferAdmission)
        catchUp(forceFresh = true)
        assertFinal(fresh())
        // An independent protected lookup verifies the exact uncached-capable sentinel route too.
        val sentinel = RoutineOccurrenceSelection(config.rootId, config.sentinelOccurrenceId)
        manager.select(sentinel)
        assertTrue(manager.load(sentinel))
        val review = requireNotNull(manager.state.value.reviewed)
        assertEquals(config.sentinelInstanceId, review.aggregate.manifest.id)
        assertInitial(review.aggregate)
        if (store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp) catchUp()
        assertRecovered()
    }

    private suspend fun fresh(): RoutineOccurrenceSnapshot {
        repeat(3) {
            manager.select(selected)
            assertTrue(manager.load(selected))
            val reviewed = requireNotNull(manager.state.value.reviewed)
            assertEquals(config.instanceId, reviewed.aggregate.manifest.id)
            assertEquals(config.rootId, reviewed.aggregate.manifest.seriesItemId)
            assertEquals(config.occurrenceId, reviewed.aggregate.manifest.occurrenceId)
            assertTrue(reviewed.freshEditEligible)
            assertManifest(reviewed.aggregate)
            if (!store.state.value.routineOccurrenceLedger.needsRemoteScheduleCatchUp) return reviewed
            catchUp()
        }
        error("Disposable occurrence evidence did not stabilize")
    }

    private suspend fun queue(letter: String, reviewed: RoutineOccurrenceSnapshot, memberId: String,
        action: RoutineOccurrenceAction,
    ): PendingRoutineOccurrenceMutation {
        assertTrue(manager.stage(selected, reviewed, memberId, action))
        val saved = durable().routineOccurrenceLedger.pending.single()
        assertNull(saved.submittedAt)
        assertEquals(config.instanceId, saved.instanceId)
        assertEquals(memberId, saved.memberId)
        assertEquals(reviewed.aggregate.revision, saved.request.expectedInstanceRevision)
        assertEquals(member(reviewed.aggregate, memberId).revision, saved.request.expectedMemberRevision)
        assertEquals(reviewed.evidenceHash, saved.request.expectedEvidenceHash)
        assertEquals(action, saved.request.action)
        writeNativeCompletionPrivateBytes(directory.resolve("operation-$letter.json"), saved.requestJson)
        return saved
    }

    private suspend fun catchUp(forceFresh: Boolean = false) {
        assertTrue(manager.refresh())
        val before = durable().routineOccurrenceLedger
        assertTrue(before.minimumCatchUpRevisions.isEmpty())
        assertTrue(before.pending.isEmpty())
        assertFalse(before.deltaCursor.isNullOrBlank())
        assertNull(durable().pendingSchedulePublication)
        if (before.needsRemoteScheduleCatchUp || forceFresh) {
            val previousOperations = credentials.publicationOperations.toList()
            if (before.needsRemoteScheduleCatchUp) {
                assertTrue(manager.catchUpSchedule { canonical.refreshRoutineOccurrenceSchedule() })
            } else {
                assertNotNull(canonical.refreshRoutineOccurrenceSchedule())
            }
            assertTrue("Catch-up must actually dispatch a new publication operation", credentials.publicationOperations.size > previousOperations.size)
            assertTrue(credentials.publicationOperations.drop(previousOperations.size).all { it !in previousOperations })
        }
        assertFalse(durable().routineOccurrenceLedger.needsRemoteScheduleCatchUp)
        assertNull(durable().pendingSchedulePublication)
        assertNotNull(durable().publishedScheduleProof)
        assertNull(durable().localScheduleCompositionProvenance)
        assertTemplatesUnchangedIfPrepared()
    }

    private suspend fun assertReceiptTarget(revision: Long) {
        val ledger = durable().routineOccurrenceLedger
        assertTrue(ledger.pending.isEmpty())
        assertEquals(mapOf(config.instanceId to revision), ledger.minimumCatchUpRevisions)
        assertTrue(ledger.needsRemoteScheduleCatchUp)
        assertEquals(revision, occurrence(config.instanceId).revision)
        assertNull(manager.state.value.reviewed)
    }

    private suspend fun assertRecovered() {
        val ledger = durable().routineOccurrenceLedger
        assertTrue(ledger.pending.isEmpty())
        assertTrue(ledger.minimumCatchUpRevisions.isEmpty())
        assertFalse(ledger.needsRemoteScheduleCatchUp)
        assertFalse(ledger.deltaCursor.isNullOrBlank())
        assertEquals(requireNotNull(disk.read("baseline_sentinel")), ITEM_PROGRESS_JSON.encodeToString(occurrence(config.sentinelInstanceId)))
        assertTemplatesUnchanged()
    }

    fun assertCommon(state: DayWeaveUiState) {
        assertEquals(config.itemIds, state.canonicalItems.map { it.id }.toSet())
        assertEquals(6, state.canonicalItems.size)
        assertTrue(state.canonicalItems.all { it.deletedAt == null })
        assertTrue(state.canonicalRecentlyDeleted.isEmpty())
        val items = state.canonicalItems.associateBy { it.id }
        assertEquals("routine", items.getValue(config.rootId).kind)
        assertNotNull(items.getValue(config.rootId).recurrenceJson)
        assertNull(items.getValue(config.rootId).parentId)
        assertEquals(config.rootId, items.getValue(config.branchId).parentId)
        assertEquals(config.branchId, items.getValue(config.requiredLeafId).parentId)
        listOf(config.optionalLeafId, config.inboxLeafId, config.blockedLeafId).forEach {
            assertEquals(config.rootId, items.getValue(it).parentId)
        }
        assertEquals("inbox", items.getValue(config.inboxLeafId).status)
        assertEquals("blocked", items.getValue(config.blockedLeafId).status)
        assertEquals(CanonicalBlockedReasonKind.MANUAL, items.getValue(config.blockedLeafId).blockedReasonKind)
        assertEquals(ROUTINE_WAIT_REASON, items.getValue(config.blockedLeafId).blockedReason)
        assertTrue(state.pendingCanonicalAuthoringMutations.isEmpty())
        assertNull(state.pendingCanonicalMutation)
        assertNull(state.pendingExecutionCommand)
        assertNull(state.pendingExecutionDeferIntent)
        assertNull(state.canonicalExecutionSession)
        assertEquals(0L, state.canonicalExecutionRevision)
        assertNull(state.activeSession)
        assertNull(state.localScheduleCompositionProvenance)
        assertTrue(state.itemCompletionLedger.pending.isEmpty())
        assertEquals(ItemProgressLedger(), state.itemProgressLedger)
        assertManifest(state.routineOccurrenceLedger.observations.getValue(config.instanceId).snapshot.aggregate)
        assertManifest(state.routineOccurrenceLedger.observations.getValue(config.sentinelInstanceId).snapshot.aggregate)
    }

    private fun assertManifest(aggregate: RoutineOccurrenceAggregate) {
        aggregate.requireValid()
        assertEquals(config.rootId, aggregate.manifest.seriesItemId)
        assertEquals(config.itemIds, aggregate.manifest.members.map { it.itemId }.toSet())
        assertEquals(6, aggregate.manifest.members.size)
        val definitions = aggregate.manifest.members.associateBy { it.itemId }
        assertEquals("inbox", definitions.getValue(config.inboxLeafId).initialOpen.status)
        assertEquals(ItemCompletionReopening("blocked", "manual", null, ROUTINE_WAIT_REASON), definitions.getValue(config.blockedLeafId).initialOpen)
        listOf(config.optionalLeafId, config.inboxLeafId, config.blockedLeafId).forEach {
            assertFalse(definitions.getValue(it).requiredForParent)
            assertFalse(member(aggregate, it).requiredForParent)
        }
        assertTrue(member(aggregate, config.branchId).requiredForParent)
        assertTrue(member(aggregate, config.requiredLeafId).requiredForParent)
    }

    private fun assertInitial(aggregate: RoutineOccurrenceAggregate) {
        assertManifest(aggregate)
        assertEquals(1L, aggregate.revision)
        assertTrue(aggregate.members.all { it.revision == 1L })
        listOf(config.rootId, config.branchId, config.requiredLeafId, config.optionalLeafId).forEach {
            assertEquals("planned", member(aggregate, it).status)
        }
        assertEquals("inbox", member(aggregate, config.inboxLeafId).status)
        assertEquals("blocked", member(aggregate, config.blockedLeafId).status)
        assertEquals(ItemCompletionCounts(2, 0, 2, 0), aggregate.validatedCounts().second.getValue(config.rootId))
    }

    private fun assertCompletedBranch(aggregate: RoutineOccurrenceAggregate) {
        assertEquals("completed", member(aggregate, config.branchId).status)
        assertEquals("completed", member(aggregate, config.requiredLeafId).status)
        assertEquals("planned", member(aggregate, config.optionalLeafId).status)
        assertEquals("inbox", member(aggregate, config.inboxLeafId).status)
        assertEquals("blocked", member(aggregate, config.blockedLeafId).status)
    }

    private fun assertFinal(snapshot: RoutineOccurrenceSnapshot) {
        assertEquals(6L, snapshot.aggregate.revision)
        assertManifest(snapshot.aggregate)
        assertCompletedBranch(snapshot.aggregate)
        val root = member(snapshot.aggregate, config.rootId)
        assertEquals("completed", root.status)
        assertEquals(ItemCompletionMode.AUTOMATIC, root.mode)
        assertEquals(ItemCompletionProvenanceKind.AUTOMATIC, root.provenance?.kind)
        assertNotNull(root.completedAt)
        assertEquals(ItemCompletionCounts(2, 2, 0, 0), snapshot.members.single { it.itemId == config.rootId }.counts)
        assertEquals(ItemCompletionReopening("blocked", "manual", null, ROUTINE_WAIT_REASON), member(snapshot.aggregate, config.blockedLeafId).open)
    }

    private fun assertTemplatesUnchangedIfPrepared() {
        if (disk.read("prepared_canonical") != null) assertTemplatesUnchanged()
    }
    private fun assertTemplatesUnchanged() = assertEquals(requireNotNull(disk.read("prepared_canonical")),
        ITEM_PROGRESS_JSON.encodeToString(store.state.value.canonicalItems.sortedBy { it.id }))
    private suspend fun durable() = requireNotNull(repository.load())
    private fun occurrence(instance: String) = store.state.value.routineOccurrenceLedger.observations.getValue(instance).snapshot.aggregate
    private fun member(aggregate: RoutineOccurrenceAggregate, item: String) = aggregate.members.single { it.itemId == item }
}

private class ObservedRoutineTransport(private val real: RoutineOccurrenceTransport) : RoutineOccurrenceTransport {
    val getInstances = mutableListOf<String>()
    val lookups = mutableListOf<RoutineOccurrenceSelection>()
    val putBodies = mutableListOf<String>()
    val failures = mutableListOf<RoutineOccurrenceFailureCode>()
    var actualPuts = 0
    var failBeforeWire = false
    var failPages = false
    var beforePut: suspend (String, String, String) -> Unit = { _, _, _ -> }
    override suspend fun lookup(configuration: AuthenticatedApiConfiguration, seriesItemId: String, occurrenceId: String): RoutineOccurrenceSnapshot {
        lookups += RoutineOccurrenceSelection(seriesItemId, occurrenceId)
        return real.lookup(configuration, seriesItemId, occurrenceId)
    }
    override suspend fun get(configuration: AuthenticatedApiConfiguration, instanceId: String): RoutineOccurrenceSnapshot {
        getInstances += instanceId
        return real.get(configuration, instanceId)
    }
    override suspend fun put(configuration: AuthenticatedApiConfiguration, instanceId: String, memberId: String, requestJson: String): RoutineOccurrenceMutationResult {
        beforePut(instanceId, memberId, requestJson)
        putBodies += requestJson
        if (failBeforeWire) throw IOException("Synthetic failure before disposable routine PUT")
        actualPuts++
        return try { real.put(configuration, instanceId, memberId, requestJson) }
        catch (error: RoutineOccurrenceApiException.Definitive) { failures += error.code; throw error }
    }
    override suspend fun list(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int): RoutineOccurrencePage {
        if (failPages) throw IOException("Synthetic unavailable occurrence page")
        return real.list(configuration, cursor, limit)
    }
    override suspend fun delta(configuration: AuthenticatedApiConfiguration, cursor: String?, limit: Int): RoutineOccurrencePage {
        if (failPages) throw IOException("Synthetic unavailable occurrence page")
        return real.delta(configuration, cursor, limit)
    }
}

private class NativeRoutineCredentials(private val config: NativeRoutineConfig, private val phase: String) :
    ApiCredentialStore, DeviceAuthRequestExecutor {
    private val gate = ApiBindingOperationGate()
    val publicationOperations = mutableListOf<String>()
    var beforePublication: suspend (String) -> Unit = {}
    override fun snapshot() = ApiConnectionSnapshot(config.baseUrl, true, null, config.configurationId)
    override fun authenticatedConfiguration() = AuthenticatedApiConfiguration.createCoordinated(
        config.baseUrl, config.bearerToken, config.configurationId, this, gate, allowCleartextLoopback = true)
    override fun update(baseUrl: String, bearerToken: String?) = error("Synthetic binding is immutable")
    override fun clear() = error("Synthetic binding is retained")
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
    override suspend fun executeAuthenticated(configuration: AuthenticatedApiConfiguration, client: OkHttpClient, request: Request): Response {
        require(configuration.baseUrl.toString() == config.baseUrl && configuration.configurationId == config.configurationId)
        require(request.url.scheme == "http" && request.url.host == "127.0.0.1" && request.url.port == configuration.baseUrl.port &&
            request.url.username.isEmpty() && request.url.password.isEmpty())
        val path = request.url.encodedPath
        val canonicalRead = request.method == "GET" && path == "/v1/items/delta" &&
            request.url.queryParameterNames.all { it in setOf("limit", "cursor", "bootstrap") }
        val occurrencePage = request.method == "GET" && path in setOf("/v1/routine-occurrences", "/v1/routine-occurrences/delta") &&
            request.url.queryParameterNames.all { it in setOf("limit", "cursor") }
        val occurrenceRead = request.method == "GET" && path in setOf("/v1/routine-occurrences/${config.instanceId}",
            "/v1/routine-occurrences/${config.sentinelInstanceId}") && request.url.query == null
        val lookup = request.method == "GET" && path == "/v1/routine-occurrences/lookup" &&
            request.url.queryParameterNames == setOf("series_item_id", "occurrence_id") &&
            request.url.queryParameter("series_item_id") == config.rootId &&
            request.url.queryParameter("occurrence_id") in setOf(config.occurrenceId, config.sentinelOccurrenceId)
        val allowedMembers = when (phase) {
            "conflict_keep_open" -> setOf(config.requiredLeafId, config.rootId)
            "finish" -> setOf(config.rootId, config.blockedLeafId)
            else -> emptySet()
        }
        val occurrenceWrite = request.method == "PUT" && request.url.query == null && allowedMembers.any {
            path == "/v1/routine-occurrences/${config.instanceId}/members/$it"
        }
        val schedule = request.method == "POST" && request.url.query == null &&
            path in setOf("/v1/schedule/preview", "/v1/schedule/publish")
        require(canonicalRead || occurrencePage || occurrenceRead || lookup || occurrenceWrite || schedule) {
            "Request outside disposable routine scope"
        }
        if (path == "/v1/schedule/publish") {
            val body = Buffer().also { requireNotNull(request.body).writeTo(it) }.readUtf8()
            require(body.toByteArray(Charsets.UTF_8).size <= 2_097_152)
            val operation = ITEM_PROGRESS_JSON.parseToJsonElement(body).jsonObject.getValue("idempotency_key").jsonPrimitive.content
            requireCanonicalUuid(operation, "synthetic publication operation")
            beforePublication(body)
            publicationOperations += operation
        }
        return client.newCall(request).awaitDeviceAuthResponse()
    }
}

private class NativeRoutineConfig private constructor(
    val runId: String, val baseUrl: String, val bearerToken: String, val workDirectory: Path,
    val rootId: String, val branchId: String, val requiredLeafId: String, val optionalLeafId: String,
    val inboxLeafId: String, val blockedLeafId: String, val occurrenceId: String, val sentinelOccurrenceId: String,
    val instanceId: String, val sentinelInstanceId: String, val asOf: Instant, val binding: String,
) {
    val configurationId = "native-routine-android-$runId"
    val itemIds = setOf(rootId, branchId, requiredLeafId, optionalLeafId, inboxLeafId, blockedLeafId)
    override fun toString() = "NativeRoutineConfig(<redacted>)"
    companion object {
        fun read(path: Path): NativeRoutineConfig {
            requirePrivateConvergencePath(path)
            require(Files.size(path) in 1..65_536)
            val bytes = Files.readAllBytes(path)
            require(bytes.size in 1..65_536)
            val raw = String(bytes, Charsets.UTF_8)
            requireStrictItemProgressJson(raw)
            val root = ITEM_PROGRESS_JSON.parseToJsonElement(raw).jsonObject
            require(root.keys == setOf("schema_version", "run_id", "base_url", "bearer_token", "work_directory", "root_id", "branch_id",
                "required_leaf_id", "optional_leaf_id", "inbox_leaf_id", "blocked_leaf_id", "occurrence_id", "sentinel_occurrence_id",
                "instance_id", "sentinel_instance_id", "as_of"))
            require(root.getValue("schema_version") == JsonPrimitive(1))
            fun string(key: String) = root.getValue(key).jsonPrimitive.also { require(it.isString) }.content
            val runId = string("run_id").also { requireCanonicalUuid(it, "synthetic run identity") }
            val baseUrl = string("base_url")
            val uri = URI(baseUrl)
            require(uri.scheme == "http" && uri.host == "127.0.0.1" && uri.port in 1..65535 && uri.userInfo == null &&
                uri.rawQuery == null && uri.rawFragment == null && uri.rawPath == "/" && baseUrl == "http://127.0.0.1:${uri.port}/")
            val bearer = string("bearer_token").also { require(it.matches(Regex("^dw_da1_[A-Za-z0-9_-]{43}$"))) }
            val work = Path.of(string("work_directory"))
            requirePrivateConvergencePath(work, directory = true)
            require(work.toRealPath() == work && path.parent.toRealPath() == work &&
                work.parent == Path.of("/tmp").toRealPath() &&
                work.fileName.toString().matches(Regex("dayweave-native-routine\\.[A-Za-z0-9_]{6,32}")))
            val ids = listOf("root_id", "branch_id", "required_leaf_id", "optional_leaf_id", "inbox_leaf_id", "blocked_leaf_id",
                "occurrence_id", "sentinel_occurrence_id", "instance_id", "sentinel_instance_id").map { key ->
                string(key).also { requireCanonicalUuid(it, "synthetic routine identity") }
            }
            require(ids.toSet().size == ids.size)
            listOf(ids[6], ids[7]).forEach { require(UUID.fromString(it).version() == 5 && UUID.fromString(it).variant() == 2) }
            val asOfText = string("as_of")
            val asOf = Instant.parse(asOfText)
            require(asOf.nano == 0 && asOf.toString() == asOfText)
            require(Duration.between(asOf, Instant.now()).seconds in 0..300) { "Disposable routine clock is stale" }
            return NativeRoutineConfig(runId, baseUrl, bearer, work, ids[0], ids[1], ids[2], ids[3], ids[4], ids[5], ids[6], ids[7],
                ids[8], ids[9], asOf, convergenceSha256(raw))
        }
    }
}

private val ROUTINE_PHASES = setOf("prepare_offline", "conflict_keep_open", "finish", "verify")
private const val ROUTINE_WAIT_REASON = "Synthetic waiting for input"
