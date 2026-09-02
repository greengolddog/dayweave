package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/** Android's deliberately inbound-only Google collection role surface. */
enum class GoogleInboundCollectionRole {
    OFF,
    READ_ONLY,
    BLOCKING,
}

@Serializable
enum class RemoteGoogleCollectionKind {
    @SerialName("calendar")
    CALENDAR,

    @SerialName("task_list")
    TASK_LIST,
}

/** Complete server enum. Android never sends [WRITABLE] from this inbound-only transport. */
@Serializable
enum class RemoteGoogleSyncRole {
    @SerialName("read_only")
    READ_ONLY,

    @SerialName("blocking")
    BLOCKING,

    @SerialName("writable")
    WRITABLE,
}

@Serializable
enum class RemoteGoogleEventDisposition {
    @SerialName("ignore")
    IGNORE,

    @SerialName("visible_nonblocking")
    VISIBLE_NONBLOCKING,

    @SerialName("blocking")
    BLOCKING,
}

@Serializable
data class RemoteGoogleCalendarPolicy(
    @SerialName("confirmed_busy")
    val confirmedBusy: RemoteGoogleEventDisposition,
    val tentative: RemoteGoogleEventDisposition,
    val free: RemoteGoogleEventDisposition,
    @SerialName("all_day")
    val allDay: RemoteGoogleEventDisposition,
    @SerialName("publish_all_day")
    val publishAllDay: Boolean,
    @SerialName("publish_tentative")
    val publishTentative: Boolean,
    @SerialName("publish_free")
    val publishFree: Boolean,
) {
    internal val isInboundOnly: Boolean
        get() = !publishAllDay && !publishTentative && !publishFree

    companion object {
        fun inboundDefault(): RemoteGoogleCalendarPolicy = RemoteGoogleCalendarPolicy(
            confirmedBusy = RemoteGoogleEventDisposition.BLOCKING,
            tentative = RemoteGoogleEventDisposition.VISIBLE_NONBLOCKING,
            free = RemoteGoogleEventDisposition.VISIBLE_NONBLOCKING,
            allDay = RemoteGoogleEventDisposition.VISIBLE_NONBLOCKING,
            publishAllDay = false,
            publishTentative = false,
            publishFree = false,
        )
    }
}

@Serializable
enum class RemoteGoogleCalendarProjectionState {
    @SerialName("uninitialized")
    UNINITIALIZED,

    @SerialName("complete")
    COMPLETE,

    @SerialName("failed")
    FAILED,
}

@Serializable
data class RemoteGoogleSyncCollection(
    val id: String,
    @SerialName("account_id") val accountId: String,
    val kind: RemoteGoogleCollectionKind,
    @SerialName("remote_collection_id") val remoteCollectionId: String,
    @SerialName("display_name") val displayName: String,
    @SerialName("provider_access_role") val providerAccessRole: String?,
    @SerialName("provider_primary") val providerPrimary: Boolean,
    @SerialName("provider_selected") val providerSelected: Boolean,
    @SerialName("provider_hidden") val providerHidden: Boolean,
    @SerialName("provider_deleted") val providerDeleted: Boolean,
    val selected: Boolean,
    val visible: Boolean,
    @SerialName("sync_role") val syncRole: RemoteGoogleSyncRole,
    @SerialName("calendar_policy") val calendarPolicy: RemoteGoogleCalendarPolicy,
    val revision: Long,
    @SerialName("discovered_at") val discoveredAt: String,
    @SerialName("configured_at") val configuredAt: String?,
    @SerialName("last_import_at") val lastImportAt: String?,
    @SerialName("planning_projection_state")
    val planningProjectionState: RemoteGoogleCalendarProjectionState,
    @SerialName("planning_generation") val planningGeneration: Long,
    @SerialName("planning_collection_revision") val planningCollectionRevision: Long?,
    @SerialName("planning_window_start") val planningWindowStart: String?,
    @SerialName("planning_window_end") val planningWindowEnd: String?,
    @SerialName("planning_window_refreshed_at") val planningWindowRefreshedAt: String?,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
data class RemoteGoogleCollections(val collections: List<RemoteGoogleSyncCollection>)

@Serializable
enum class RemoteGoogleSyncRunState {
    @SerialName("idle")
    IDLE,

    @SerialName("running")
    RUNNING,

    @SerialName("backoff")
    BACKOFF,

    @SerialName("reauthorization_required")
    REAUTHORIZATION_REQUIRED,

    @SerialName("failed")
    FAILED,
}

@Serializable
data class RemoteGoogleSyncRunStatus(
    @SerialName("account_id") val accountId: String,
    val state: RemoteGoogleSyncRunState,
    @SerialName("requested_at") val requestedAt: String?,
    @SerialName("started_at") val startedAt: String?,
    @SerialName("completed_at") val completedAt: String?,
    @SerialName("next_attempt_at") val nextAttemptAt: String,
    @SerialName("consecutive_failures") val consecutiveFailures: Long,
    @SerialName("last_error_code") val lastErrorCode: String?,
    @SerialName("last_error_at") val lastErrorAt: String?,
    @SerialName("imported_count") val importedCount: Long,
    @SerialName("updated_count") val updatedCount: Long,
    @SerialName("deleted_count") val deletedCount: Long,
    @SerialName("conflict_count") val conflictCount: Long,
    @SerialName("rejected_count") val rejectedCount: Long,
    @SerialName("refresh_generation") val refreshGeneration: Long,
    @SerialName("claimed_refresh_generation") val claimedRefreshGeneration: Long,
    @SerialName("completed_refresh_generation") val completedRefreshGeneration: Long,
    val revision: Long,
)

@Serializable
data class RemoteGoogleSyncStatus(
    val run: RemoteGoogleSyncRunStatus?,
    @SerialName("import_conflicts") val importConflicts: Long,
    @SerialName("pending_outbound") val pendingOutbound: Long,
    @SerialName("conflicted_outbound") val conflictedOutbound: Long,
    @SerialName("failed_outbound") val failedOutbound: Long,
    @SerialName("last_outbound_error_code") val lastOutboundErrorCode: String?,
    @SerialName("last_outbound_error_at") val lastOutboundErrorAt: String?,
    @SerialName("next_outbound_attempt_at") val nextOutboundAttemptAt: String?,
)

@Serializable
data class RemoteGoogleSyncRefreshAccepted(
    @SerialName("account_id") val accountId: String,
    @SerialName("request_id") val requestId: String,
    @SerialName("refresh_generation") val refreshGeneration: Long,
    @SerialName("requested_at") val requestedAt: String,
)

data class ConfigureGoogleCollectionRequest(
    val expectedRevision: Long,
    /** Kind observed on the authoritative collection being configured. */
    val kind: RemoteGoogleCollectionKind,
    val role: GoogleInboundCollectionRole,
    val visible: Boolean = true,
    val calendarPolicy: RemoteGoogleCalendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
)

@Serializable
private data class ConfigureGoogleCollectionWireRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
    val selected: Boolean,
    val visible: Boolean,
    @SerialName("sync_role") val syncRole: RemoteGoogleSyncRole,
    @SerialName("calendar_policy") val calendarPolicy: RemoteGoogleCalendarPolicy,
)

@Serializable
private data class RemoteGoogleCollectionEnvelope(val collection: RemoteGoogleSyncCollection)

@Serializable
private data class RemoteGoogleSyncStatusEnvelope(val sync: RemoteGoogleSyncStatus)

@Serializable
private data class GoogleSyncRefreshRequest(@SerialName("request_id") val requestId: String)

@Serializable
private data class RemoteGoogleSyncRefreshEnvelope(val refresh: RemoteGoogleSyncRefreshAccepted)

sealed class GoogleCalendarInboundApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication :
        GoogleCalendarInboundApiException("The DayWeave API rejected the bearer token")

    class NotFound :
        GoogleCalendarInboundApiException("The Google Calendar resource was not found")

    class Conflict :
        GoogleCalendarInboundApiException("The Google Calendar configuration changed")

    class Validation(val statusCode: Int) : GoogleCalendarInboundApiException(
        "The DayWeave API rejected the Google Calendar request with HTTP $statusCode",
    )

    class Upstream :
        GoogleCalendarInboundApiException("Google Calendar could not be reached by the server")

    class Unavailable :
        GoogleCalendarInboundApiException("Google Calendar sync is unavailable")

    class Http(val statusCode: Int) : GoogleCalendarInboundApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : GoogleCalendarInboundApiException(
        "The DayWeave API returned an unreadable Google Calendar response",
        cause,
    )
}

interface GoogleCalendarInboundTransport {
    suspend fun collections(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections

    suspend fun discover(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections

    suspend fun configure(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ): RemoteGoogleSyncCollection

    suspend fun syncStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleSyncStatus

    suspend fun refresh(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        requestId: UUID,
    ): RemoteGoogleSyncRefreshAccepted
}

class OkHttpGoogleCalendarInboundTransport(
    private val client: OkHttpClient = OkHttpGoogleAccountsTransport.defaultClient(),
    private val json: Json = defaultJson(),
) : GoogleCalendarInboundTransport {
    override suspend fun collections(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections {
        requireCanonicalUuid(accountId, "Google account ID")
        val url = accountUrl(configuration, accountId)
            .addPathSegment("collections")
            .build()
        return execute<RemoteGoogleCollections>(
            requestBuilder(configuration, url.toString()).get().build(),
            expectedStatus = 200,
        ).also { validateCollections(it, accountId) }
    }

    override suspend fun discover(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections {
        requireCanonicalUuid(accountId, "Google account ID")
        val url = accountUrl(configuration, accountId)
            .addPathSegments("collections/discover")
            .build()
        return execute<RemoteGoogleCollections>(
            requestBuilder(configuration, url.toString())
                .post(EMPTY_JSON_BODY)
                .build(),
            expectedStatus = 200,
        ).also { validateCollections(it, accountId) }
    }

    override suspend fun configure(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ): RemoteGoogleSyncCollection {
        requireCanonicalUuid(accountId, "Google account ID")
        requireCanonicalUuid(collectionId, "Google collection ID")
        require(request.expectedRevision in 1 until Long.MAX_VALUE) {
            "Google collection revision is outside the supported range"
        }
        require(request.calendarPolicy.isInboundOnly) {
            "Android inbound configuration cannot enable Calendar publication"
        }
        require(request.hasSupportedInboundRole) {
            "The selected Google collection kind does not support this inbound role"
        }
        val wireRequest = request.toWireRequest()
        val body = json.encodeToString(wireRequest).toRequestBody(JSON_MEDIA_TYPE)
        val url = accountUrl(configuration, accountId)
            .addPathSegment("collections")
            .addPathSegment(collectionId)
            .build()
        val collection = execute<RemoteGoogleCollectionEnvelope>(
            requestBuilder(configuration, url.toString()).put(body).build(),
            expectedStatus = 200,
        ).collection
        validateCollection(collection)
        if (
            collection.id != collectionId ||
            collection.accountId != accountId ||
            collection.kind != request.kind ||
            collection.revision != request.expectedRevision + 1 ||
            collection.selected != wireRequest.selected ||
            collection.visible != wireRequest.visible ||
            collection.syncRole != wireRequest.syncRole ||
            collection.calendarPolicy != wireRequest.calendarPolicy
        ) {
            throw GoogleCalendarInboundApiException.InvalidResponse()
        }
        return collection
    }

    override suspend fun syncStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleSyncStatus {
        requireCanonicalUuid(accountId, "Google account ID")
        val url = accountUrl(configuration, accountId).addPathSegment("sync").build()
        return execute<RemoteGoogleSyncStatusEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
            expectedStatus = 200,
        ).sync.also { validateSyncStatus(it, accountId) }
    }

    override suspend fun refresh(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        requestId: UUID,
    ): RemoteGoogleSyncRefreshAccepted {
        requireCanonicalUuid(accountId, "Google account ID")
        require(requestId != ZERO_UUID) { "Google refresh request ID must not be zero" }
        val canonicalRequestId = requestId.toString()
        val body = json.encodeToString(GoogleSyncRefreshRequest(canonicalRequestId))
            .toRequestBody(JSON_MEDIA_TYPE)
        val url = accountUrl(configuration, accountId).addPathSegments("sync/refresh").build()
        return execute<RemoteGoogleSyncRefreshEnvelope>(
            requestBuilder(configuration, url.toString()).post(body).build(),
            expectedStatus = 202,
        ).refresh.also { refresh ->
            validateRefresh(refresh)
            if (refresh.accountId != accountId || refresh.requestId != canonicalRequestId) {
                throw GoogleCalendarInboundApiException.InvalidResponse()
            }
        }
    }

    private fun accountUrl(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ) = configuration.baseUrl.newBuilder()
        .addPathSegments("v1/integrations/google/accounts")
        .addPathSegment(accountId)

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")
        .header("Cache-Control", "no-store")
        .header("Pragma", "no-cache")

    private suspend inline fun <reified T> execute(
        request: Request,
        expectedStatus: Int,
    ): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw GoogleCalendarInboundApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != expectedStatus) throw response.toInboundApiException()
            if (!response.hasStrictJsonMediaType() || !response.hasStrictNoStore()) {
                throw GoogleCalendarInboundApiException.InvalidResponse()
            }
            if (response.body.contentLength() > MAX_RESPONSE_BYTES) {
                throw GoogleCalendarInboundApiException.InvalidResponse()
            }
            val responseText = response.body.charStream().use { it.readBoundedInboundText() }
            try {
                if (StrictGoogleJsonObjectKeyScanner(responseText, json).hasDuplicateKeys()) {
                    throw GoogleCalendarInboundApiException.InvalidResponse()
                }
                return json.decodeFromString<T>(responseText)
            } catch (error: GoogleCalendarInboundApiException.InvalidResponse) {
                throw error
            } catch (error: SerializationException) {
                throw GoogleCalendarInboundApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw GoogleCalendarInboundApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toInboundApiException(): GoogleCalendarInboundApiException = when (code) {
        401 -> GoogleCalendarInboundApiException.Authentication()
        404 -> GoogleCalendarInboundApiException.NotFound()
        409 -> GoogleCalendarInboundApiException.Conflict()
        400, 422 -> GoogleCalendarInboundApiException.Validation(code)
        502 -> GoogleCalendarInboundApiException.Upstream()
        503 -> GoogleCalendarInboundApiException.Unavailable()
        else -> GoogleCalendarInboundApiException.Http(code)
    }

    private fun Response.hasStrictJsonMediaType(): Boolean {
        val value = headers.values("Content-Type").singleOrNull() ?: return false
        val mediaType = value.toMediaTypeOrNull() ?: return false
        if (mediaType.type != "application" || mediaType.subtype != "json") return false
        val components = value.split(';').map { it.trim().lowercase() }
        return components.firstOrNull() == "application/json" &&
            (components.size == 1 ||
                components.size == 2 && components[1].replace(" ", "") == "charset=utf-8")
    }

    private fun Response.hasStrictNoStore(): Boolean =
        headers.values("Cache-Control").singleOrNull()?.trim()?.lowercase() == "no-store"

    private fun Reader.readBoundedInboundText(): String {
        val result = StringBuilder()
        val buffer = CharArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val read = read(buffer)
            if (read < 0) break
            if (result.length + read > MAX_RESPONSE_CHARS) {
                throw GoogleCalendarInboundApiException.InvalidResponse()
            }
            result.append(buffer, 0, read)
        }
        return result.toString()
    }

    companion object {
        private const val MAX_RESPONSE_BYTES = 16L * 1024L * 1024L
        private const val MAX_RESPONSE_CHARS = 16 * 1024 * 1024
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
        private val EMPTY_JSON_BODY = ByteArray(0).toRequestBody(null)
        private val ZERO_UUID = UUID(0, 0)

        fun defaultJson(): Json = Json {
            ignoreUnknownKeys = false
            // Nullable response members are required on the wire, even when they are null.
            explicitNulls = true
            encodeDefaults = true
        }
    }
}

private fun ConfigureGoogleCollectionRequest.toWireRequest(): ConfigureGoogleCollectionWireRequest =
    when (role) {
        GoogleInboundCollectionRole.OFF -> ConfigureGoogleCollectionWireRequest(
            expectedRevision = expectedRevision,
            selected = false,
            visible = false,
            syncRole = RemoteGoogleSyncRole.READ_ONLY,
            calendarPolicy = calendarPolicy,
        )

        GoogleInboundCollectionRole.READ_ONLY -> ConfigureGoogleCollectionWireRequest(
            expectedRevision = expectedRevision,
            selected = true,
            visible = visible,
            syncRole = RemoteGoogleSyncRole.READ_ONLY,
            calendarPolicy = calendarPolicy,
        )

        GoogleInboundCollectionRole.BLOCKING -> ConfigureGoogleCollectionWireRequest(
            expectedRevision = expectedRevision,
            selected = true,
            visible = visible,
            syncRole = RemoteGoogleSyncRole.BLOCKING,
            calendarPolicy = calendarPolicy,
        )
    }

internal val ConfigureGoogleCollectionRequest.hasSupportedInboundRole: Boolean
    get() = when (kind) {
        RemoteGoogleCollectionKind.CALENDAR -> true
        RemoteGoogleCollectionKind.TASK_LIST -> role != GoogleInboundCollectionRole.BLOCKING
    }

private fun validateCollections(response: RemoteGoogleCollections, accountId: String) {
    if (response.collections.size > MAX_GOOGLE_COLLECTIONS) {
        throw GoogleCalendarInboundApiException.InvalidResponse()
    }
    val ids = HashSet<String>(response.collections.size)
    val remoteIdentities = HashSet<Pair<RemoteGoogleCollectionKind, String>>(
        response.collections.size,
    )
    response.collections.forEach { collection ->
        validateCollection(collection)
        if (
            collection.accountId != accountId ||
            !ids.add(collection.id) ||
            !remoteIdentities.add(collection.kind to collection.remoteCollectionId)
        ) {
            throw GoogleCalendarInboundApiException.InvalidResponse()
        }
    }
}

private fun validateCollection(collection: RemoteGoogleSyncCollection) {
    val id = canonicalUuidOrNull(collection.id)
    val accountId = canonicalUuidOrNull(collection.accountId)
    val discoveredAt = instantOrNull(collection.discoveredAt)
    val configuredAt = collection.configuredAt?.let(::instantOrNull)
    val lastImportAt = collection.lastImportAt?.let(::instantOrNull)
    val windowStart = collection.planningWindowStart?.let(::instantOrNull)
    val windowEnd = collection.planningWindowEnd?.let(::instantOrNull)
    val windowRefreshedAt = collection.planningWindowRefreshedAt?.let(::instantOrNull)
    val createdAt = instantOrNull(collection.createdAt)
    val updatedAt = instantOrNull(collection.updatedAt)
    val roleIsValid = when (collection.kind to collection.syncRole) {
        RemoteGoogleCollectionKind.CALENDAR to RemoteGoogleSyncRole.READ_ONLY,
        RemoteGoogleCollectionKind.CALENDAR to RemoteGoogleSyncRole.BLOCKING,
        RemoteGoogleCollectionKind.CALENDAR to RemoteGoogleSyncRole.WRITABLE,
        RemoteGoogleCollectionKind.TASK_LIST to RemoteGoogleSyncRole.READ_ONLY,
        RemoteGoogleCollectionKind.TASK_LIST to RemoteGoogleSyncRole.WRITABLE,
        -> true

        RemoteGoogleCollectionKind.TASK_LIST to RemoteGoogleSyncRole.BLOCKING -> false
        else -> false
    }
    val writableCalendarAccessIsValid =
        collection.kind != RemoteGoogleCollectionKind.CALENDAR ||
            collection.syncRole != RemoteGoogleSyncRole.WRITABLE ||
            collection.providerAccessRole in setOf("owner", "writer")
    val publicationPolicyIsValid =
        collection.syncRole == RemoteGoogleSyncRole.WRITABLE || collection.calendarPolicy.isInboundOnly
    val projectionRevisionIsValid = collection.planningCollectionRevision?.let {
        it > 0
    } ?: true
    val windowIsValid = when {
        windowStart == null && windowEnd == null ->
            collection.planningWindowStart == null && collection.planningWindowEnd == null

        windowStart != null && windowEnd != null -> windowStart.isBefore(windowEnd)
        else -> false
    }
    val valid = id != null && id != ZERO_GOOGLE_UUID &&
        accountId != null && accountId != ZERO_GOOGLE_UUID &&
        collection.revision > 0 &&
        collection.planningGeneration >= 0 &&
        projectionRevisionIsValid &&
        validGoogleInboundText(collection.remoteCollectionId, 2_048) &&
        validGoogleInboundText(collection.displayName, 4_096) &&
        (collection.providerAccessRole?.let { validGoogleInboundText(it, 64) } ?: true) &&
        discoveredAt != null &&
        (collection.configuredAt == null || configuredAt != null) &&
        (collection.lastImportAt == null || lastImportAt != null) &&
        (collection.planningWindowRefreshedAt == null || windowRefreshedAt != null) &&
        createdAt != null && updatedAt != null && !createdAt.isAfter(updatedAt) &&
        roleIsValid && writableCalendarAccessIsValid && publicationPolicyIsValid && windowIsValid
    if (!valid) throw GoogleCalendarInboundApiException.InvalidResponse()
}

private fun validateSyncStatus(status: RemoteGoogleSyncStatus, accountId: String) {
    val counts = listOf(
        status.importConflicts,
        status.pendingOutbound,
        status.conflictedOutbound,
        status.failedOutbound,
    )
    val outboundTimesAreValid =
        (status.lastOutboundErrorAt == null || instantOrNull(status.lastOutboundErrorAt) != null) &&
            (status.nextOutboundAttemptAt == null ||
                instantOrNull(status.nextOutboundAttemptAt) != null)
    val valid = counts.all { it >= 0 } &&
        (status.lastOutboundErrorCode?.let { validGoogleInboundText(it, 256) } ?: true) &&
        outboundTimesAreValid
    if (!valid) throw GoogleCalendarInboundApiException.InvalidResponse()
    status.run?.let { validateSyncRun(it, accountId) }
}

private fun validateSyncRun(run: RemoteGoogleSyncRunStatus, accountId: String) {
    val counts = listOf(
        run.importedCount,
        run.updatedCount,
        run.deletedCount,
        run.conflictCount,
        run.rejectedCount,
    )
    val optionalTimesAreValid = listOf(
        run.requestedAt,
        run.startedAt,
        run.completedAt,
        run.lastErrorAt,
    ).all { it == null || instantOrNull(it) != null }
    val valid = canonicalUuidOrNull(run.accountId) != null &&
        run.accountId == accountId &&
        run.revision > 0 &&
        run.consecutiveFailures in 0..MAX_UNSIGNED_INT &&
        counts.all { it >= 0 } &&
        run.refreshGeneration >= 0 &&
        run.claimedRefreshGeneration in 0..run.refreshGeneration &&
        run.completedRefreshGeneration in 0..run.claimedRefreshGeneration &&
        (run.lastErrorCode?.let { validGoogleInboundText(it, 256) } ?: true) &&
        instantOrNull(run.nextAttemptAt) != null && optionalTimesAreValid
    if (!valid) throw GoogleCalendarInboundApiException.InvalidResponse()
}

private fun validateRefresh(refresh: RemoteGoogleSyncRefreshAccepted) {
    val valid = canonicalUuidOrNull(refresh.accountId)?.let { it != ZERO_GOOGLE_UUID } == true &&
        canonicalUuidOrNull(refresh.requestId)?.let { it != ZERO_GOOGLE_UUID } == true &&
        refresh.refreshGeneration > 0 &&
        instantOrNull(refresh.requestedAt) != null
    if (!valid) throw GoogleCalendarInboundApiException.InvalidResponse()
}

private fun requireCanonicalUuid(value: String, description: String) {
    require(canonicalUuidOrNull(value)?.let { it != ZERO_GOOGLE_UUID } == true) {
        "$description is invalid"
    }
}

private fun canonicalUuidOrNull(value: String): UUID? = try {
    UUID.fromString(value).takeIf { it.toString() == value }
} catch (_: IllegalArgumentException) {
    null
}

private fun instantOrNull(value: String): Instant? = try {
    Instant.parse(value)
} catch (_: DateTimeParseException) {
    null
}

private fun validGoogleInboundText(value: String, maximumUtf8Bytes: Int): Boolean =
    value.isNotEmpty() &&
        StandardCharsets.UTF_8.newEncoder().canEncode(value) &&
        value.toByteArray(StandardCharsets.UTF_8).size <= maximumUtf8Bytes &&
        value.none(Char::isISOControl)

/** Detects duplicate object keys, including equivalent escaped spellings, before decoding. */
private class StrictGoogleJsonObjectKeyScanner(
    private val source: String,
    private val json: Json,
) {
    private var index = 0

    fun hasDuplicateKeys(): Boolean {
        skipWhitespace()
        val duplicate = parseValue(depth = 0)
        skipWhitespace()
        require(index == source.length)
        return duplicate
    }

    private fun parseValue(depth: Int): Boolean {
        require(depth <= MAX_JSON_DEPTH)
        skipWhitespace()
        require(index < source.length)
        return when (source[index]) {
            '{' -> parseObject(depth)
            '[' -> parseArray(depth)
            '"' -> {
                parseString()
                false
            }

            else -> {
                parsePrimitive()
                false
            }
        }
    }

    private fun parseObject(depth: Int): Boolean {
        index += 1
        skipWhitespace()
        if (takeIfPresent('}')) return false
        val keys = hashSetOf<String>()
        var duplicate = false
        while (true) {
            skipWhitespace()
            require(source.getOrNull(index) == '"')
            if (!keys.add(parseString())) duplicate = true
            skipWhitespace()
            require(takeIfPresent(':'))
            if (parseValue(depth + 1)) duplicate = true
            skipWhitespace()
            when {
                takeIfPresent('}') -> return duplicate
                takeIfPresent(',') -> Unit
                else -> throw IllegalArgumentException("Invalid JSON object")
            }
        }
    }

    private fun parseArray(depth: Int): Boolean {
        index += 1
        skipWhitespace()
        if (takeIfPresent(']')) return false
        var duplicate = false
        while (true) {
            if (parseValue(depth + 1)) duplicate = true
            skipWhitespace()
            when {
                takeIfPresent(']') -> return duplicate
                takeIfPresent(',') -> Unit
                else -> throw IllegalArgumentException("Invalid JSON array")
            }
        }
    }

    private fun parseString(): String {
        val start = index
        require(takeIfPresent('"'))
        while (index < source.length) {
            when (source[index++]) {
                '"' -> return json.decodeFromString(source.substring(start, index))
                '\\' -> {
                    require(index < source.length)
                    if (source[index++] == 'u') {
                        repeat(4) {
                            require(source.getOrNull(index)?.isHexDigit() == true)
                            index += 1
                        }
                    }
                }
            }
        }
        throw IllegalArgumentException("Unterminated JSON string")
    }

    private fun parsePrimitive() {
        val start = index
        while (index < source.length && source[index] !in PRIMITIVE_DELIMITERS) index += 1
        require(index > start)
    }

    private fun skipWhitespace() {
        while (index < source.length && source[index] in JSON_WHITESPACE) index += 1
    }

    private fun takeIfPresent(character: Char): Boolean {
        if (source.getOrNull(index) != character) return false
        index += 1
        return true
    }

    private fun Char.isHexDigit(): Boolean =
        this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

    private companion object {
        const val MAX_JSON_DEPTH = 64
        val JSON_WHITESPACE = setOf(' ', '\t', '\r', '\n')
        val PRIMITIVE_DELIMITERS = JSON_WHITESPACE + setOf(',', ']', '}')
    }
}

private const val MAX_GOOGLE_COLLECTIONS = 10_000
private const val MAX_UNSIGNED_INT = 4_294_967_295L
private val ZERO_GOOGLE_UUID = UUID(0, 0)
