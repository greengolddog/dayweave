package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.RoomPlannerStateRepository
import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.net.Proxy
import java.net.URI
import java.nio.file.Files
import java.nio.file.Path
import java.time.Instant
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/** Opt-in real-service test. The root harness owns the disposable PostgreSQL/service lifecycle. */
class NativeProgressConvergenceTest {
    @Test fun runSelectedPhaseAgainstDisposableService() = runBlocking {
        val rawConfigPath = System.getenv("DAYWEAVE_NATIVE_CONVERGENCE_CONFIG")
        val rawPhase = System.getenv("DAYWEAVE_NATIVE_CONVERGENCE_PHASE")
        assumeTrue("Requires explicit disposable-service config and phase", nativeConvergenceOptIn(rawConfigPath, rawPhase))
        val phase = requireNotNull(rawPhase)
        require(phase in setOf("prepare", "conflict_update", "verify"))
        // Parser diagnostics must never quote the private JSON containing the synthetic bearer.
        val config = try { ConvergenceConfig.read(Path.of(requireNotNull(rawConfigPath))) }
        catch (_: Exception) { throw AssertionError("Invalid private disposable-service convergence configuration") }
        val androidDirectory = config.workDirectory.resolve("android")
        val disk = NativeConvergenceDisk(androidDirectory, config.binding, phase == "prepare")
        val repository = RoomPlannerStateRepository(NativeConvergenceSnapshotDao(disk))
        val credentials = ConvergenceCredentials(config)
        val configuration = requireNotNull(credentials.authenticatedConfiguration())
        val client = OkHttpCanonicalPlannerTransport.defaultClient().newBuilder()
            .proxy(Proxy.NO_PROXY).followRedirects(false).followSslRedirects(false)
            .callTimeout(15, TimeUnit.SECONDS).build()
        val transport = ObservedProgressTransport(OkHttpItemProgressTransport(client))
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            if (phase == "prepare") {
                val canonical = readCanonical(config, configuration, client)
                disk.write("canonical_baseline", ITEM_PROGRESS_JSON.encodeToString(canonical.records))
                repository.save(canonical.initialState(config.baseUrl, config.configurationId))
            } else {
                require(disk.read("canonical_baseline") != null && disk.read("snapshot") != null)
            }
            val store = PlannerStore(DayWeaveUiState(), repository, scope)
            withTimeout(15_000) { store.loadState.first { it != PlannerLoadState.LOADING } }
            assertEquals(PlannerLoadState.READY, store.loadState.value)
            val baseline = store.state.value
            assertEquals(config.baseUrl, baseline.canonicalSyncOrigin)
            assertEquals(config.configurationId, baseline.canonicalConfigurationId)
            assertFalse("Complete canonical hydration must retain the real terminal delta cursor",
                baseline.canonicalDeltaCursor.isNullOrBlank())
            val goal = requireNotNull(baseline.progressItem(config.itemId))
            val manager = ItemProgressSyncManager(store, credentials, transport)
            val details = linkedMapOf<String, JsonElement>()
            when (phase) {
                "prepare" -> {
                    assertTrue(manager.load(config.itemId))
                    assertObservation(store, config, 0, emptyList())
                    // Explicit offline review: stage never invokes a PUT or starts a replay worker.
                    assertTrue(manager.stage(config.itemId, goal.revision, 0, config.replacementComponents))
                    val pending = requireNotNull(repository.load()).itemProgressLedger.pending.single()
                    assertNull(pending.submittedAt)
                    assertEquals(ItemProgressDisposition.PENDING, pending.disposition)
                    assertEquals(config.replacementComponents, pending.request().components)
                    assertTrue(transport.putBodies.isEmpty())
                    disk.write("prepared_request", pending.requestJson)
                    details["operation_id"] = JsonPrimitive(pending.operationId)
                    details["request_sha256"] = JsonPrimitive(convergenceSha256(pending.requestJson))
                }
                "conflict_update" -> {
                    val original = baseline.itemProgressLedger.pending.single()
                    assertEquals(requireNotNull(disk.read("prepared_request")), original.requestJson)
                    assertNull(original.submittedAt)
                    assertObservation(store, config, 0, emptyList())
                    transport.beforePut = { body ->
                        val durable = requireNotNull(repository.load()).itemProgressLedger.pending.single()
                        assertNotNull(durable.submittedAt)
                        assertEquals(body, durable.requestJson)
                    }
                    assertTrue(manager.replay())
                    assertEquals(listOf(ItemProgressFailureCode.PROGRESS_STALE), transport.definitiveFailures)
                    assertEquals(listOf(original.requestJson), transport.putBodies)
                    val rejected = requireNotNull(repository.load()).itemProgressLedger.pending.single()
                    assertEquals(original.operationId, rejected.operationId)
                    assertEquals(original.requestJson, rejected.requestJson)
                    assertNotNull(rejected.submittedAt)
                    assertEquals(ItemProgressDisposition.REVIEW_REQUIRED, rejected.disposition)
                    assertTrue(manager.replay())
                    assertEquals(1, transport.putBodies.size)
                    assertTrue(manager.load(config.itemId))
                    assertObservation(store, config, 1, config.initialComponents)
                    assertEquals(rejected, requireNotNull(repository.load()).itemProgressLedger.pending.single())
                    // This is a new explicit review, not an automatic rebasing of operation B.
                    assertTrue(manager.stage(config.itemId, goal.revision, 1, config.replacementComponents,
                        replacingOperationId = original.operationId))
                    val replacement = requireNotNull(repository.load()).itemProgressLedger.pending.single()
                    assertNotEquals(original.operationId, replacement.operationId)
                    assertNotEquals(original.requestJson, replacement.requestJson)
                    assertNull(replacement.submittedAt)
                    assertTrue(manager.replay())
                    assertTrue(requireNotNull(repository.load()).itemProgressLedger.pending.isEmpty())
                    val result = transport.results.single()
                    assertFalse(result.replayed)
                    assertEquals(2L, result.progress.revision)
                    assertEquals(config.replacementComponents, result.progress.components)
                    assertFalse(store.state.value.itemProgressLedger.observations.getValue(config.itemId).isGetProof)
                    details["rejected_operation_id"] = JsonPrimitive(original.operationId)
                    details["rejected_request_sha256"] = JsonPrimitive(convergenceSha256(original.requestJson))
                    details["operation_id"] = JsonPrimitive(replacement.operationId)
                    details["request_sha256"] = JsonPrimitive(convergenceSha256(replacement.requestJson))
                    details["definitive_conflict"] = JsonPrimitive("item_progress_revision_stale")
                }
                "verify" -> {
                    assertTrue(baseline.itemProgressLedger.pending.isEmpty())
                    assertEquals(2L, baseline.itemProgressLedger.observations.getValue(config.itemId).snapshot.revision)
                    assertTrue(manager.load(config.itemId))
                    assertObservation(store, config, 2, config.replacementComponents)
                    assertTrue(transport.putBodies.isEmpty())
                }
            }
            // Every phase checks local side effects and the actual canonical server records.
            assertEquals(baseline, store.state.value.copy(itemProgressLedger = baseline.itemProgressLedger))
            val remote = readCanonical(config, configuration, client).records
            val canonicalJson = ITEM_PROGRESS_JSON.encodeToString(remote)
            assertEquals(requireNotNull(disk.read("canonical_baseline")), canonicalJson)
            assertEquals(baseline.canonicalItems, remote.map(::admitSyntheticCanonicalItem))
            val durable = requireNotNull(repository.load())
            assertEquals(store.state.value.itemProgressLedger, durable.itemProgressLedger)
            val report = buildJsonObject {
                put("schema_version", 1)
                put("run_id", config.runId)
                put("phase", requireNotNull(phase))
                put("status", "passed")
                put("canonical_sha256", convergenceSha256(canonicalJson))
                put("progress_revision", durable.itemProgressLedger.observations.getValue(config.itemId).snapshot.revision)
                put("pending_count", durable.itemProgressLedger.pending.size)
                put("http_request_count", credentials.requestCount)
                details.forEach { (key, value) -> put(key, value) }
            }
            atomicPrivateConvergenceWrite(androidDirectory.resolve("$phase.json"), report.toString().toByteArray(Charsets.UTF_8))
        } finally {
            scope.cancel()
            client.dispatcher.executorService.shutdown()
            client.connectionPool.evictAll()
        }
    }

    private fun assertObservation(store: PlannerStore, config: ConvergenceConfig, revision: Long,
        components: List<ItemProgressComponent>,
    ) {
        val observation = store.state.value.itemProgressLedger.observations.getValue(config.itemId)
        assertTrue(observation.isGetProof)
        assertEquals(revision, observation.snapshot.revision)
        assertEquals(components, observation.snapshot.components)
        assertEquals(store.state.value.progressItem(config.itemId)?.revision, observation.snapshot.itemRevision)
    }

    private suspend fun readCanonical(config: ConvergenceConfig, configuration: AuthenticatedApiConfiguration,
        client: OkHttpClient,
    ): NativeConvergenceCanonicalRead {
        val transport = OkHttpCanonicalPlannerTransport(client)
        val items = linkedMapOf<String, RemoteCanonicalItem>()
        val seenCursors = mutableSetOf<String?>()
        var cursor: String? = null
        var pages = 0
        var changes = 0
        do {
            require(++pages <= 512 && seenCursors.add(cursor))
            val page = transport.itemDelta(configuration, cursor)
            require(page.changes.size <= maximumItemDeltaResponseChanges(50))
            require(page.nextCursor.isNotBlank() && page.nextCursor.length <= 4_096 &&
                page.nextCursor.none(Char::isISOControl))
            require(!page.hasMore || page.nextCursor !in seenCursors)
            changes += page.changes.size
            require(changes <= 512 * 50)
            page.changes.forEach { foldNativeConvergenceCanonicalChange(items, it) }
            cursor = page.nextCursor
        } while (page.hasMore)
        val records = items.values.sortedBy { it.id }
        assertEquals(setOf(config.itemId, config.childId), records.map { it.id }.toSet())
        val goal = records.single { it.id == config.itemId }
        val child = records.single { it.id == config.childId }
        assertEquals("goal", goal.kind)
        assertFalse(requireNotNull(goal.hasOwnEffort))
        assertFalse(goal.isExecutable)
        assertNull(goal.parentId)
        assertEquals("task", child.kind)
        assertEquals(goal.id, child.parentId)
        assertTrue(records.all { it.deletedAt == null && !it.isSensitive })
        return NativeConvergenceCanonicalRead(records, requireNotNull(cursor))
    }
}

/** The terminal read proof belongs to the admitted snapshot and must survive encrypted restart. */
internal data class NativeConvergenceCanonicalRead(
    val records: List<RemoteCanonicalItem>,
    val terminalCursor: String,
) {
    fun initialState(syncOrigin: String, configurationId: String): DayWeaveUiState {
        require(terminalCursor.isNotBlank() && terminalCursor.length <= 4_096 &&
            terminalCursor.none(Char::isISOControl))
        return DayWeaveUiState(
            canonicalItems = records.map(::admitSyntheticCanonicalItem),
            canonicalSyncOrigin = syncOrigin,
            canonicalConfigurationId = configurationId,
            canonicalDeltaCursor = terminalCursor,
        )
    }
}

/** Only a completely absent opt-in skips; a partial or blank setup is a harness failure. */
internal fun nativeConvergenceOptIn(configPath: String?, phase: String?): Boolean {
    if (configPath == null && phase == null) return false
    require(!configPath.isNullOrBlank() && !phase.isNullOrBlank()) {
        "Disposable-service convergence requires both nonblank config and phase"
    }
    return true
}

/** Mirrors CanonicalSyncManager.applyDeltaChange; a delta is a sequence, not a unique-ID snapshot. */
internal fun foldNativeConvergenceCanonicalChange(
    items: MutableMap<String, RemoteCanonicalItem>,
    change: RemoteItemDeltaChange,
) {
    when (change.type) {
        "upsert" -> {
            require(change.tombstone == null)
            val incoming = requireNotNull(change.item)
            requireCanonicalUuid(incoming.id, "synthetic canonical item")
            require(incoming.revision > 0 && incoming.deletedAt == null)
            val mapped = admitSyntheticCanonicalItem(incoming)
            val existing = items[incoming.id]
            require(existing == null || incoming.revision >= existing.revision)
            require(existing == null || incoming.revision != existing.revision ||
                mapped == admitSyntheticCanonicalItem(existing)) { "Conflicting equal canonical revision" }
            items[incoming.id] = incoming
        }
        "tombstone" -> {
            require(change.item == null)
            val tombstone = requireNotNull(change.tombstone)
            requireCanonicalUuid(tombstone.id, "synthetic canonical tombstone")
            tombstone.parentId?.let { requireCanonicalUuid(it, "synthetic tombstone parent") }
            Instant.parse(tombstone.deletedAt)
            require(tombstone.revision > 0)
            require(items[tombstone.id]?.let { tombstone.revision > it.revision } != false)
            items.remove(tombstone.id)
        }
        else -> error("Unknown synthetic canonical delta variant")
    }
}

/** No lifecycle/schedule/provider managers run: only exact fixture admission from the real delta. */
private fun admitSyntheticCanonicalItem(remote: RemoteCanonicalItem) = CanonicalItemSnapshot(
    id = remote.id, isSensitive = remote.isSensitive, kind = remote.kind, status = remote.status,
    title = remote.title, notes = remote.notes, timezoneName = remote.timezoneName,
    durationSeconds = remote.durationSeconds, durationKind = requireNotNull(remote.durationKind),
    durationMinSeconds = remote.durationMinSeconds, durationMaxSeconds = remote.durationMaxSeconds,
    durationSource = remote.durationSource, deadlineAt = remote.deadlineAt,
    deadlineKind = requireNotNull(remote.deadlineKind), deadlineDate = remote.deadlineDate,
    deadlineStrength = remote.deadlineStrength, deadlineSoftWeight = remote.deadlineSoftWeight,
    earliestStartAt = remote.earliestStartAt, recurrenceJson = remote.recurrence?.toString(),
    flexibleConstraintsJson = remote.flexibleConstraints.toString(), splitPolicyJson = remote.splitPolicy.toString(),
    importance = remote.importance, urgency = remote.urgency, parentId = remote.parentId,
    siblingOrder = remote.siblingOrder, hasOwnEffort = requireNotNull(remote.hasOwnEffort),
    blockedReasonKind = remote.blockedReasonKind, blockedByItemId = remote.blockedByItemId,
    blockedReason = remote.blockedReason, isExecutable = remote.isExecutable, revision = remote.revision,
    createdAt = remote.createdAt, updatedAt = remote.updatedAt, completedAt = remote.completedAt,
    deletedAt = remote.deletedAt, hasExplicitStructuralMetadata = true,
).let { it.copy(hasExplicitStructuralMetadata = !it.hasLegacyEquivalentStructuralMetadata()) }
    .also { it.requireValidStructuralMetadata() }

private class ObservedProgressTransport(private val real: ItemProgressTransport) : ItemProgressTransport {
    val putBodies = mutableListOf<String>()
    val definitiveFailures = mutableListOf<ItemProgressFailureCode>()
    val results = mutableListOf<ItemProgressMutationResult>()
    var beforePut: suspend (String) -> Unit = {}
    override suspend fun get(configuration: AuthenticatedApiConfiguration, itemId: String) = real.get(configuration, itemId)
    override suspend fun put(configuration: AuthenticatedApiConfiguration, itemId: String, requestJson: String): ItemProgressMutationResult {
        beforePut(requestJson)
        putBodies += requestJson
        return try { real.put(configuration, itemId, requestJson).also(results::add) }
        catch (error: ItemProgressApiException.Definitive) { definitiveFailures += error.code; throw error }
    }
}

private class ConvergenceConfig private constructor(
    val runId: String,
    val baseUrl: String,
    val bearerToken: String,
    val itemId: String,
    val childId: String,
    val workDirectory: Path,
    val initialComponents: List<ItemProgressComponent>,
    val replacementComponents: List<ItemProgressComponent>,
    val binding: String,
) {
    val configurationId = "native-convergence-android-$runId"
    companion object {
        fun read(path: Path): ConvergenceConfig {
            requirePrivateConvergencePath(path)
            require(Files.size(path) in 1..65_536)
            val raw = Files.readAllBytes(path).toString(Charsets.UTF_8)
            requireStrictItemProgressJson(raw)
            val root = ITEM_PROGRESS_JSON.parseToJsonElement(raw).jsonObject
            require(root.keys == setOf("schema_version", "run_id", "base_url", "bearer_token", "item_id",
                "child_id", "work_directory", "initial_components", "replacement_components"))
            require(root.getValue("schema_version") == JsonPrimitive(1))
            fun string(name: String) = root.getValue(name).jsonPrimitive.also { require(it.isString) }.content
            val runId = string("run_id").also { require(it.matches(Regex("[A-Za-z0-9_-]{1,80}"))) }
            val baseUrl = string("base_url")
            val uri = URI(baseUrl)
            require(uri.scheme == "http" && uri.host == "127.0.0.1" && uri.port in 1..65535 &&
                uri.userInfo == null && uri.rawQuery == null && uri.rawFragment == null && uri.rawPath == "/" &&
                baseUrl == "http://127.0.0.1:${uri.port}/") { "Only an explicit literal loopback origin is permitted" }
            val itemId = string("item_id").also { requireCanonicalUuid(it, "synthetic goal") }
            val childId = string("child_id").also { requireCanonicalUuid(it, "synthetic child") }
            require(itemId != childId)
            val work = Path.of(string("work_directory"))
            requirePrivateConvergencePath(work, directory = true)
            require(work.toRealPath() == work && path.parent.toRealPath() == work)
            fun components(name: String) = decodeExactItemProgress<List<ItemProgressComponent>>(root.getValue(name).toString())
                .also { requireProgressComponents(it); require(it.size == 3) }
            val initial = components("initial_components")
            val replacement = components("replacement_components")
            require(initial.map { it.id } == replacement.map { it.id } && initial != replacement)
            require(initial.map { it.value::class }.toSet().size == 3)
            require(initial.zip(replacement).all { (left, right) -> left.value::class == right.value::class })
            return ConvergenceConfig(runId, baseUrl, string("bearer_token"), itemId, childId, work,
                initial, replacement, convergenceSha256(raw))
        }
    }
}

/** Every request is actual HTTP; this executor also prevents proxy/redirect/other-endpoint escape. */
private class ConvergenceCredentials(private val config: ConvergenceConfig) : ApiCredentialStore, DeviceAuthRequestExecutor {
    var requestCount = 0
        private set
    private val gate = ApiBindingOperationGate()
    override fun snapshot() = ApiConnectionSnapshot(config.baseUrl, true, null, config.configurationId)
    override fun authenticatedConfiguration() = AuthenticatedApiConfiguration.createCoordinated(
        config.baseUrl, config.bearerToken, config.configurationId, this, gate, allowCleartextLoopback = true,
    )
    override fun update(baseUrl: String, bearerToken: String?) = error("Synthetic binding cannot change")
    override fun clear() = error("Synthetic binding cannot be abandoned")
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
    override suspend fun executeAuthenticated(configuration: AuthenticatedApiConfiguration,
        client: OkHttpClient, request: Request,
    ): Response {
        require(configuration.baseUrl.toString() == config.baseUrl && configuration.configurationId == config.configurationId)
        require(request.url.scheme == "http" && request.url.host == "127.0.0.1" &&
            request.url.port == configuration.baseUrl.port && request.url.username.isEmpty() && request.url.password.isEmpty())
        val canonicalRead = request.method == "GET" && request.url.encodedPath == "/v1/items/delta"
        val progress = request.method in setOf("GET", "PUT") && request.url.encodedPath == "/v1/items/${config.itemId}/progress" &&
            request.url.query == null
        require(canonicalRead || progress) { "Convergence may only read fixture canonical state or access its own progress" }
        requestCount++
        return client.newCall(request).awaitDeviceAuthResponse()
    }
}
