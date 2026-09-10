package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.*
import com.greengolddog.dayweave.network.*
import java.net.URI
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.*
import java.nio.file.attribute.PosixFilePermissions
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okio.Buffer

internal val NATIVE_COMPLETION_PHASES = setOf("prepare_offline", "conflict_keep_open",
    "catchup_automatic_optional", "verify_cascade_and_child")
internal const val COMPLETION_WAIT_REASON = "Synthetic waiting for input"

internal class NativeCompletionConfig private constructor(
    val runId: String, val baseUrl: String, val bearerToken: String, val workDirectory: Path,
    val rootId: String, val branchId: String, val requiredLeafId: String, val optionalLeafId: String,
    val newChildId: String, val binding: String,
) {
    val configurationId = "native-completion-android-$runId"
    val initialIds = setOf(rootId, branchId, requiredLeafId, optionalLeafId)
    val finalIds = initialIds + newChildId
    override fun toString() = "NativeCompletionConfig(<redacted>)"

    companion object {
        fun read(path: Path): NativeCompletionConfig {
            requirePrivateConvergencePath(path)
            require(Files.size(path) in 1..65_536)
            val raw = Files.readAllBytes(path).toString(Charsets.UTF_8)
            requireStrictItemProgressJson(raw)
            val root = ITEM_PROGRESS_JSON.parseToJsonElement(raw).jsonObject
            require(root.keys == setOf("schema_version", "run_id", "base_url", "bearer_token", "work_directory",
                "root_id", "branch_id", "required_leaf_id", "optional_leaf_id", "new_child_id"))
            require(root.getValue("schema_version") == JsonPrimitive(1))
            fun string(key: String) = root.getValue(key).jsonPrimitive.also { require(it.isString) }.content
            val runId = string("run_id").also { require(it.matches(Regex("[A-Za-z0-9_-]{1,80}"))) }
            val baseUrl = string("base_url")
            val uri = URI(baseUrl)
            require(uri.scheme == "http" && uri.host == "127.0.0.1" && uri.port in 1..65535 &&
                uri.userInfo == null && uri.rawQuery == null && uri.rawFragment == null && uri.rawPath == "/" &&
                baseUrl == "http://127.0.0.1:${uri.port}/")
            val bearer = string("bearer_token").also {
                require(it.matches(Regex("^dw_da1_[A-Za-z0-9_-]{43}$")))
            }
            val work = Path.of(string("work_directory"))
            requirePrivateConvergencePath(work, directory = true)
            require(work.toRealPath() == work && path.parent.toRealPath() == work &&
                work.fileName.toString().startsWith("dayweave-native-completion."))
            val ids = listOf("root_id", "branch_id", "required_leaf_id", "optional_leaf_id", "new_child_id").map { key ->
                string(key).also { requireCanonicalUuid(it, "synthetic completion identity") }
            }
            require(ids.toSet().size == 5)
            return NativeCompletionConfig(runId, baseUrl, bearer, work, ids[0], ids[1], ids[2], ids[3], ids[4], convergenceSha256(raw))
        }
    }
}

/** Actual loopback HTTP only; no redirect, proxy, provider, execution or unrelated item writes. */
internal class NativeCompletionCredentials(private val config: NativeCompletionConfig, private val phase: String) :
    ApiCredentialStore, DeviceAuthRequestExecutor {
    private val gate = ApiBindingOperationGate()
    var requestCount = 0
        private set
    var beforeCreate: suspend (String, String) -> Unit = { _, _ -> }
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
        val completionRead = request.method == "GET" && config.finalIds.any { path == "/v1/items/$it/completion" } && request.url.query == null
        val completionWrite = request.method == "PUT" && request.url.query == null &&
            (path == "/v1/items/${config.rootId}/completion" && phase in setOf("conflict_keep_open", "catchup_automatic_optional") ||
                path == "/v1/items/${config.optionalLeafId}/completion" && phase == "catchup_automatic_optional")
        val schedule = phase == "verify_cascade_and_child" && request.method == "POST" && request.url.query == null &&
            path in setOf("/v1/schedule/preview", "/v1/schedule/publish")
        val create = phase == "verify_cascade_and_child" && request.method == "POST" && path == "/v1/items" && request.url.query == null
        require(canonicalRead || completionRead || completionWrite || schedule || create) { "Request outside disposable completion scope" }
        if (create) {
            val body = Buffer().also { requireNotNull(request.body).writeTo(it) }.readUtf8()
            require(body.toByteArray(Charsets.UTF_8).size <= 65_536)
            val root = ITEM_PROGRESS_JSON.parseToJsonElement(body).jsonObject
            require(root["id"] == JsonPrimitive(config.newChildId) && root["parent_id"] == JsonPrimitive(config.branchId) &&
                root["status"] == JsonPrimitive("planned") && root["kind"] == JsonPrimitive("task"))
            beforeCreate(requireNotNull(request.header("Idempotency-Key")), body)
        }
        requestCount++
        return client.newCall(request).awaitDeviceAuthResponse()
    }
}

internal fun nativeCompletionItems(items: List<CanonicalItemSnapshot>): JsonArray = JsonArray(items.sortedBy { it.id }.map { item ->
    buildJsonObject {
        put("id", item.id); put("revision", item.revision); put("status", item.status); put("parent_id", item.parentId)
        put("blocked_reason_kind", item.blockedReasonKind?.wireValue); put("blocked_by_item_id", item.blockedByItemId)
        put("blocked_reason", item.blockedReason)
    }
})

/** A second invocation must never overwrite a successful phase marker. */
internal fun writeNativeCompletionMarker(path: Path, marker: JsonObject) {
    writeNativeCompletionPrivateBytes(path, marker.toString())
}

internal fun writeNativeCompletionPrivateBytes(path: Path, value: String) {
    requirePrivateConvergencePath(path.parent, directory = true)
    val bytes = value.toByteArray(Charsets.UTF_8).also { require(it.size <= 131_072) }
    FileChannel.open(path, setOf(CREATE_NEW, WRITE), PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rw-------"))).use {
        val buffer = ByteBuffer.wrap(bytes)
        while (buffer.hasRemaining()) it.write(buffer)
        it.force(true)
    }
    FileChannel.open(path.parent, READ).use { it.force(true) }
    requirePrivateConvergencePath(path)
}
