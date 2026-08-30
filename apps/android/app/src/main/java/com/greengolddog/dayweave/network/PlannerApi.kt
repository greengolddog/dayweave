package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.time.Instant
import java.util.UUID
import java.util.concurrent.TimeUnit
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
data class RemoteCanonicalItem(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val kind: String,
    val status: String,
    val title: String,
    val notes: String? = null,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("duration_seconds") val durationSeconds: Long? = null,
    @SerialName("deadline_at") val deadlineAt: String? = null,
    @SerialName("earliest_start_at") val earliestStartAt: String? = null,
    val recurrence: JsonElement? = null,
    @SerialName("flexible_constraints") val flexibleConstraints: JsonObject,
    @SerialName("split_policy") val splitPolicy: JsonObject,
    val importance: Int,
    val urgency: Int,
    @SerialName("parent_id") val parentId: String? = null,
    @SerialName("sibling_order") val siblingOrder: Long,
    @SerialName("is_executable") val isExecutable: Boolean,
    val revision: Long,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
    @SerialName("completed_at") val completedAt: String? = null,
    @SerialName("deleted_at") val deletedAt: String? = null,
)

@Serializable
data class RemoteItemTombstone(
    val id: String,
    val revision: Long,
    @SerialName("deleted_at") val deletedAt: String,
    @SerialName("parent_id") val parentId: String? = null,
)

@Serializable
data class RemoteItemDeltaChange(
    val type: String,
    val item: RemoteCanonicalItem? = null,
    val tombstone: RemoteItemTombstone? = null,
)

@Serializable
data class RemoteItemDeltaPage(
    val changes: List<RemoteItemDeltaChange>,
    @SerialName("next_cursor") val nextCursor: String,
    @SerialName("has_more") val hasMore: Boolean,
)

@Serializable
data class CanonicalItemReplacement(
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val kind: String,
    val status: String,
    val title: String,
    val notes: String? = null,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("duration_seconds") val durationSeconds: Long? = null,
    @SerialName("deadline_at") val deadlineAt: String? = null,
    @SerialName("earliest_start_at") val earliestStartAt: String? = null,
    val recurrence: JsonElement? = null,
    @SerialName("flexible_constraints") val flexibleConstraints: JsonObject,
    @SerialName("split_policy") val splitPolicy: JsonObject,
    val importance: Int,
    val urgency: Int,
    @SerialName("parent_id") val parentId: String? = null,
    @SerialName("sibling_order") val siblingOrder: Long,
)

/** Flat `/v1/items` create body; the server does not accept a nested `item` object here. */
@Serializable
data class CreateCanonicalItemRequest(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val kind: String,
    val status: String,
    val title: String,
    val notes: String? = null,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("duration_seconds") val durationSeconds: Long? = null,
    @SerialName("deadline_at") val deadlineAt: String? = null,
    @SerialName("earliest_start_at") val earliestStartAt: String? = null,
    val recurrence: JsonElement? = null,
    @SerialName("flexible_constraints") val flexibleConstraints: JsonObject,
    @SerialName("split_policy") val splitPolicy: JsonObject,
    val importance: Int,
    val urgency: Int,
    @SerialName("parent_id") val parentId: String? = null,
    @SerialName("sibling_order") val siblingOrder: Long,
)

@Serializable
data class ReplaceCanonicalItemRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
    val item: CanonicalItemReplacement,
)

@Serializable
data class CanonicalItemRevisionRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
)

@Serializable
private data class CanonicalItemEnvelope(val item: RemoteCanonicalItem)

@Serializable
data class ScheduleAvailabilityRequest(
    val start: String,
    val end: String,
    val contexts: Set<String> = emptySet(),
    val location: String? = null,
    val energy: String = "medium",
)

@Serializable
data class PreviousScheduleBlockRequest(
    val start: String,
    val end: String,
    @SerialName("session_index") val sessionIndex: Int,
)

@Serializable
data class PreviousScheduleAssignmentRequest(
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    val blocks: List<PreviousScheduleBlockRequest>,
    val pinned: Boolean = false,
)

@Serializable
data class ScheduleConfigRequest(
    @SerialName("slot_granularity_minutes") val slotGranularityMinutes: Int = 5,
    @SerialName("stability_weight") val stabilityWeight: Int = 4,
    @SerialName("default_soft_weight") val defaultSoftWeight: Int = 100,
)

@Serializable
data class FixedScheduleBlockRequest(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val title: String,
    val start: String,
    val end: String,
    val source: String,
)

@Serializable
data class SchedulePreviewRequest(
    @SerialName("as_of") val asOf: String,
    @SerialName("horizon_start") val horizonStart: String,
    @SerialName("horizon_end") val horizonEnd: String,
    @SerialName("timezone_name") val timezoneName: String,
    val availability: List<ScheduleAvailabilityRequest>,
    @SerialName("fixed_blocks")
    val fixedBlocks: List<FixedScheduleBlockRequest> = emptyList(),
    @SerialName("previous_assignments")
    val previousAssignments: List<PreviousScheduleAssignmentRequest> = emptyList(),
    val config: ScheduleConfigRequest = ScheduleConfigRequest(),
    @SerialName("recurrence_context") val recurrenceContext: JsonObject = JsonObject(emptyMap()),
)

@Serializable
data class SchedulePublishRequest(
    @SerialName("idempotency_key") val idempotencyKey: String,
    @SerialName("expected_input_digest") val expectedInputDigest: String,
    val schedule: SchedulePreviewRequest,
)

/** Exact non-secret publication request persisted before the first network send. */
@Serializable
data class SchedulePublishHttpRequest(
    val url: String,
    val method: String,
    @SerialName("accept_header") val acceptHeader: String,
    @SerialName("content_type_header") val contentTypeHeader: String,
    @SerialName("cache_control_header") val cacheControlHeader: String,
    @SerialName("pragma_header") val pragmaHeader: String,
    @SerialName("body_json") val bodyJson: String,
    @SerialName("body_sha256") val bodySha256: String,
) {
    override fun toString(): String =
        "SchedulePublishHttpRequest(url=$url, method=$method, body=<redacted>)"
}

@Serializable
data class RemotePublishedScheduleRevision(
    val id: String,
    val revision: String,
    @SerialName("revision_number") val revisionNumber: ULong,
    @SerialName("input_digest") val inputDigest: String,
    @SerialName("horizon_start") val horizonStart: String,
    @SerialName("horizon_end") val horizonEnd: String,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("published_at") val publishedAt: String,
)

@Serializable
data class RemoteSchedulePublishResponse(
    val revision: RemotePublishedScheduleRevision,
    val replayed: Boolean,
)

@Serializable
data class RemoteRejectedScheduleItem(
    @SerialName("item_id") val itemId: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val title: String,
    val reason: String,
)

@Serializable
data class RemoteScheduleBlock(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    @SerialName("item_id") val itemId: String? = null,
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    @SerialName("external_block_id") val externalBlockId: String? = null,
    val title: String,
    val start: String,
    val end: String,
    @SerialName("session_index") val sessionIndex: Int,
    val kind: String,
    val explanations: List<RemotePlacementExplanation> = emptyList(),
)

@Serializable
data class RemotePlacementExplanation(
    val code: String,
    val message: String,
)

@Serializable
data class RemoteUnscheduledWork(
    @SerialName("item_id") val itemId: String,
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    val remaining: Long,
    val reason: String,
    val message: String,
)

@Serializable
data class RemotePlanDecision(
    @SerialName("item_id") val itemId: String,
    @SerialName("occurrence_id") val occurrenceId: String? = null,
    val kind: String,
    val message: String,
)

@Serializable
data class RemotePlanOccurrence(
    val id: String,
    @SerialName("series_item_id") val seriesItemId: String,
    val identity: JsonObject,
    @SerialName("nominal_start") val nominalStart: String,
    @SerialName("nominal_end") val nominalEnd: String,
    @SerialName("window_start") val windowStart: String,
    @SerialName("window_end") val windowEnd: String,
    @SerialName("local_date") val localDate: String? = null,
    val ordinal: Long,
    val state: String,
)

@Serializable
data class RemoteIgnoredPreviousAssignment(
    @SerialName("item_id") val itemId: String,
    @SerialName("requested_revision") val requestedRevision: Long,
    @SerialName("current_revision") val currentRevision: Long? = null,
    val reason: String,
)

@Serializable
data class RemotePlanScore(
    @SerialName("scheduled_minutes") val scheduledMinutes: Long,
    @SerialName("unscheduled_minutes") val unscheduledMinutes: Long,
    @SerialName("soft_penalty") val softPenalty: Long,
    @SerialName("moved_minutes") val movedMinutes: Long,
)

@Serializable
data class RemotePlanViolation(
    val kind: String,
    val severity: String,
    @SerialName("item_ids") val itemIds: List<String>,
    @SerialName("occurrence_ids") val occurrenceIds: List<String> = emptyList(),
    val start: String? = null,
    val end: String? = null,
    val penalty: Long,
    val message: String,
)

@Serializable
data class RemoteSchedulePlan(
    @SerialName("as_of") val asOf: String,
    @SerialName("horizon_start") val horizonStart: String,
    @SerialName("horizon_end") val horizonEnd: String,
    val blocks: List<RemoteScheduleBlock>,
    val unscheduled: List<RemoteUnscheduledWork>,
    val decisions: List<RemotePlanDecision> = emptyList(),
    val violations: List<RemotePlanViolation> = emptyList(),
    val score: RemotePlanScore,
    val occurrences: List<RemotePlanOccurrence> = emptyList(),
)

@Serializable
data class RemoteSchedulePreview(
    @SerialName("input_digest") val inputDigest: String,
    @SerialName("source_item_count") val sourceItemCount: Int,
    @SerialName("source_item_revisions") val sourceItemRevisions: Map<String, Long>,
    @SerialName("accepted_item_count") val acceptedItemCount: Int,
    @SerialName("rejected_items") val rejectedItems: List<RemoteRejectedScheduleItem>,
    @SerialName("ignored_previous_assignments")
    val ignoredPreviousAssignments: List<RemoteIgnoredPreviousAssignment>,
    val plan: RemoteSchedulePlan,
)

sealed class PlannerApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : PlannerApiException("The DayWeave API rejected the bearer token")

    class Conflict : PlannerApiException("The canonical item changed on the server")

    /** A trusted canonical 409 proving that the exact request made no server-side change. */
    class CanonicalMutationRejected : PlannerApiException(
        "The canonical item mutation was deterministically rejected",
    )

    /** A trusted canonical 409 that is still ambiguous and must retain its exact retry journal. */
    class CanonicalMutationInProgress : PlannerApiException(
        "The matching canonical item mutation is still in progress",
    )

    class SchedulePublicationStale : PlannerApiException(
        "The validated schedule preview became stale before publication",
    )

    class Validation(val statusCode: Int) : PlannerApiException(
        "The DayWeave API rejected planner input with HTTP $statusCode",
    )

    class Http(val statusCode: Int) : PlannerApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : PlannerApiException(
        "The DayWeave API returned an unreadable planner response",
        cause,
    )
}

interface CanonicalPlannerTransport {
    suspend fun itemDelta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
    ): RemoteItemDeltaPage

    suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePreviewRequest,
    ): RemoteSchedulePreview

    suspend fun publish(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePublishHttpRequest,
    ): RemoteSchedulePublishResponse

    suspend fun createItem(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: CreateCanonicalItemRequest,
    ): RemoteCanonicalItem

    suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem

    suspend fun trashItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        expectedRevision: Long,
    ): RemoteCanonicalItem

    suspend fun restoreItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: CanonicalItemRevisionRequest,
    ): RemoteCanonicalItem
}

class OkHttpCanonicalPlannerTransport(
    private val client: OkHttpClient = defaultClient(),
    private val json: Json = defaultJson(),
) : CanonicalPlannerTransport {
    override suspend fun itemDelta(
        configuration: AuthenticatedApiConfiguration,
        cursor: String?,
    ): RemoteItemDeltaPage {
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items/delta")
            .addQueryParameter("limit", MAX_DELTA_PAGE_SIZE.toString())
            .apply { if (cursor != null) addQueryParameter("cursor", cursor) }
            .build()
        return execute(requestBuilder(configuration, url.toString()).get().build())
    }

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePreviewRequest,
    ): RemoteSchedulePreview {
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/schedule/preview")
            .build()
        val body = json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)
        return execute(requestBuilder(configuration, url.toString()).post(body).build())
    }

    override suspend fun publish(
        configuration: AuthenticatedApiConfiguration,
        request: SchedulePublishHttpRequest,
    ): RemoteSchedulePublishResponse {
        validateSchedulePublishHttpRequest(configuration, request)
        val body = request.bodyJson.toRequestBody(request.contentTypeHeader.toMediaType())
        val httpRequest = Request.Builder()
            .url(request.url)
            .tag(AuthenticatedApiConfiguration::class.java, configuration)
            .header("Accept", request.acceptHeader)
            .header("Authorization", "Bearer ${configuration.bearerToken}")
            .header("Cache-Control", request.cacheControlHeader)
            .header("Pragma", request.pragmaHeader)
            .header("Content-Type", request.contentTypeHeader)
            .post(body)
            .build()
        return executePublication(httpRequest)
    }

    override suspend fun createItem(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: CreateCanonicalItemRequest,
    ): RemoteCanonicalItem {
        requireCanonicalItemMutationIdentity(request.id, idempotencyKey)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items")
            .build()
        val body = json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)
        val httpRequest = requestBuilder(configuration, url.toString())
            .header("Idempotency-Key", idempotencyKey)
            .post(body)
            .build()
        return execute<CanonicalItemEnvelope>(httpRequest, expectedStatusCode = 201).item
    }

    override suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem {
        requireCanonicalItemMutationIdentity(id, idempotencyKey)
        requireCanonicalItemRevision(request.expectedRevision)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items")
            .addPathSegment(id)
            .build()
        val body = json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)
        val httpRequest = requestBuilder(configuration, url.toString())
            .header("Idempotency-Key", idempotencyKey)
            .put(body)
            .build()
        return execute<CanonicalItemEnvelope>(httpRequest).item
    }

    override suspend fun trashItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        expectedRevision: Long,
    ): RemoteCanonicalItem {
        requireCanonicalItemMutationIdentity(id, idempotencyKey)
        requireCanonicalItemRevision(expectedRevision)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items")
            .addPathSegment(id)
            .addQueryParameter("expected_revision", expectedRevision.toString())
            .build()
        val httpRequest = requestBuilder(configuration, url.toString())
            .header("Idempotency-Key", idempotencyKey)
            .delete()
            .build()
        return execute<CanonicalItemEnvelope>(httpRequest).item
    }

    override suspend fun restoreItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: CanonicalItemRevisionRequest,
    ): RemoteCanonicalItem {
        requireCanonicalItemMutationIdentity(id, idempotencyKey)
        requireCanonicalItemRevision(request.expectedRevision)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/items")
            .addPathSegment(id)
            .addPathSegment("restore")
            .build()
        val body = json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)
        val httpRequest = requestBuilder(configuration, url.toString())
            .header("Idempotency-Key", idempotencyKey)
            .post(body)
            .build()
        return execute<CanonicalItemEnvelope>(httpRequest).item
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(
        request: Request,
        expectedStatusCode: Int = 200,
    ): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw PlannerApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != expectedStatusCode) throw response.toPlannerApiException()
            val mediaType = response.header("Content-Type")?.toMediaTypeOrNull()
            if (mediaType?.type != "application" || mediaType.subtype != "json") {
                throw PlannerApiException.InvalidResponse()
            }
            val responseText = response.body.charStream().use { reader ->
                reader.readBoundedPlannerText()
            }
            try {
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw PlannerApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw PlannerApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.isStrictSchedulePublicationStale(): Boolean {
        if (code != 409) return false
        val mediaType = header("Content-Type")?.toMediaTypeOrNull() ?: return false
        if (mediaType.type != "application" || mediaType.subtype != "json") return false
        val responseText = runCatching {
            body.charStream().use { reader ->
                reader.readBoundedPlannerText(MAX_ERROR_RESPONSE_CHARS)
            }
        }.getOrNull() ?: return false
        val root = runCatching {
            json.parseToJsonElement(responseText).jsonObject
        }.getOrNull() ?: return false
        if (root.keys != setOf("error")) return false
        val error = runCatching { root.getValue("error").jsonObject }.getOrNull() ?: return false
        if (error.keys != setOf("code", "message")) return false
        val codePrimitive = error["code"]?.jsonPrimitive ?: return false
        val messagePrimitive = error["message"]?.jsonPrimitive ?: return false
        if (!codePrimitive.isString || !messagePrimitive.isString) return false
        val code = codePrimitive.contentOrNull ?: return false
        val message = messagePrimitive.contentOrNull ?: return false
        return code == SCHEDULE_PUBLICATION_STALE_CODE &&
            message.isNotBlank() && message.length <= MAX_ERROR_MESSAGE_CHARS
    }

    private suspend fun executePublication(request: Request): RemoteSchedulePublishResponse {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw PlannerApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != 200) {
                if (response.isStrictSchedulePublicationStale()) {
                    throw PlannerApiException.SchedulePublicationStale()
                }
                throw response.toPlannerApiException()
            }
            val mediaType = response.header("Content-Type")?.toMediaTypeOrNull()
            if (mediaType?.type != "application" || mediaType.subtype != "json") {
                throw PlannerApiException.InvalidResponse()
            }
            val responseText = response.body.charStream().use { reader ->
                reader.readBoundedPlannerText()
            }
            try {
                return json.decodeFromString(responseText)
            } catch (error: SerializationException) {
                throw PlannerApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw PlannerApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toPlannerApiException(): PlannerApiException = when (code) {
        401 -> PlannerApiException.Authentication()
        404 -> strictCanonicalMutationNotFound() ?: PlannerApiException.Http(code)
        409 -> strictCanonicalMutationConflict() ?: PlannerApiException.Conflict()
        400, 422 -> PlannerApiException.Validation(code)
        else -> PlannerApiException.Http(code)
    }

    /** A trusted item-route 404 proves that a replace/trash/restore request changed nothing. */
    private fun Response.strictCanonicalMutationNotFound(): PlannerApiException? {
        val endpoint = canonicalMutationEndpoint()?.takeUnless {
            it == CanonicalMutationEndpoint.CREATE
        } ?: return null
        val mediaType = header("Content-Type")?.toMediaTypeOrNull() ?: return null
        if (mediaType.type != "application" || mediaType.subtype != "json") return null
        if (header("Cache-Control")?.lowercase() != "no-store, max-age=0") return null
        if (header("Pragma")?.lowercase() != "no-cache") return null
        val responseText = runCatching {
            body.charStream().use { reader ->
                reader.readBoundedPlannerText(MAX_ERROR_RESPONSE_CHARS)
            }
        }.getOrNull() ?: return null
        val root = runCatching { json.parseToJsonElement(responseText).jsonObject }
            .getOrNull() ?: return null
        if (root.keys != setOf("error")) return null
        val error = runCatching { root.getValue("error").jsonObject }.getOrNull() ?: return null
        return if (
            error.keys == setOf("code", "message") &&
            error["code"]?.jsonPrimitive?.takeIf { it.isString }?.contentOrNull == "not_found" &&
            error["message"]?.jsonPrimitive?.takeIf { it.isString }?.contentOrNull ==
            "item was not found"
        ) {
            PlannerApiException.CanonicalMutationRejected()
        } else {
            null
        }
    }

    /**
     * Trust only the exact authenticated server envelopes whose outcome is unambiguous.
     * A generic or future 409 remains replayable because it may represent an in-flight request.
     */
    private fun Response.strictCanonicalMutationConflict(): PlannerApiException? {
        val endpoint = canonicalMutationEndpoint() ?: return null
        val mediaType = header("Content-Type")?.toMediaTypeOrNull() ?: return null
        if (mediaType.type != "application" || mediaType.subtype != "json") return null
        if (header("Cache-Control")?.lowercase() != "no-store, max-age=0") return null
        if (header("Pragma")?.lowercase() != "no-cache") return null
        val responseText = runCatching {
            body.charStream().use { reader ->
                reader.readBoundedPlannerText(MAX_ERROR_RESPONSE_CHARS)
            }
        }.getOrNull() ?: return null
        val root = runCatching { json.parseToJsonElement(responseText).jsonObject }
            .getOrNull() ?: return null
        if (root.keys != setOf("error")) return null
        val error = runCatching { root.getValue("error").jsonObject }.getOrNull() ?: return null
        val code = error["code"]?.jsonPrimitive?.takeIf { it.isString }?.contentOrNull
            ?: return null
        val message = error["message"]?.jsonPrimitive?.takeIf { it.isString }?.contentOrNull
            ?: return null
        if (code != "conflict" || message.isBlank() || message.length > MAX_ERROR_MESSAGE_CHARS) {
            return null
        }
        if (
            error.keys == setOf("code", "message") &&
            message == "matching idempotent request is still in progress"
        ) {
            return PlannerApiException.CanonicalMutationInProgress()
        }
        if (
            endpoint != CanonicalMutationEndpoint.CREATE &&
            error.keys == setOf("code", "message", "details") &&
            message == "item was changed by another request"
        ) {
            val details = runCatching { error.getValue("details").jsonObject }.getOrNull()
                ?: return null
            if (
                details.keys == setOf("expected_revision", "actual_revision") &&
                details.values.all { value ->
                    value.jsonPrimitive.takeUnless { it.isString }?.longOrNull?.let { it > 0 } == true
                }
            ) {
                return PlannerApiException.CanonicalMutationRejected()
            }
            return null
        }
        if (error.keys != setOf("code", "message")) return null
        val common = setOf("Idempotency-Key was already used for different content")
        val endpointSpecific = when (endpoint) {
            CanonicalMutationEndpoint.CREATE -> setOf(
                "item already exists",
                "item cannot be its own parent",
                "item hierarchy would contain a cycle",
                "an executing or terminal item cannot become a parent",
            )
            CanonicalMutationEndpoint.REPLACE -> setOf(
                "item cannot be its own parent",
                "item hierarchy would contain a cycle",
                "an executing or terminal item cannot become a parent",
                "only leaf items can enter an executable state",
            )
            CanonicalMutationEndpoint.TRASH -> setOf(
                "an item with active children cannot be deleted",
            )
            CanonicalMutationEndpoint.RESTORE -> setOf(
                "deleted item's parent must be restored first",
                "only leaf items can enter an executable state",
            )
        }
        return if (message in common || message in endpointSpecific) {
            PlannerApiException.CanonicalMutationRejected()
        } else {
            null
        }
    }

    private fun Response.canonicalMutationEndpoint(): CanonicalMutationEndpoint? {
        val segments = request.url.pathSegments
        val v1Index = segments.indexOfLast { it == "v1" }
        if (v1Index < 0 || segments.getOrNull(v1Index + 1) != "items") return null
        val suffix = segments.drop(v1Index)
        return when {
            request.method == "POST" && suffix == listOf("v1", "items") ->
                CanonicalMutationEndpoint.CREATE
            request.method == "PUT" && suffix.size == 3 -> CanonicalMutationEndpoint.REPLACE
            request.method == "DELETE" && suffix.size == 3 -> CanonicalMutationEndpoint.TRASH
            request.method == "POST" && suffix.size == 4 && suffix.last() == "restore" ->
                CanonicalMutationEndpoint.RESTORE
            else -> null
        }
    }

    companion object {
        const val MAX_DELTA_PAGE_SIZE = 50
        // One page can legitimately contain large notes plus bounded recurrence/constraint JSON.
        private const val MAX_RESPONSE_CHARS = 12 * 1024 * 1024
        private const val MAX_ERROR_RESPONSE_CHARS = 8 * 1024
        private const val MAX_ERROR_MESSAGE_CHARS = 500
        private const val SCHEDULE_PUBLICATION_STALE_CODE = "schedule_publication_stale"
        private val CANONICAL_ITEM_IDEMPOTENCY_KEY = Regex("^[A-Za-z0-9._:-]{8,128}$")
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()

        private enum class CanonicalMutationEndpoint {
            CREATE,
            REPLACE,
            TRASH,
            RESTORE,
        }

        private fun requireCanonicalItemMutationIdentity(id: String, idempotencyKey: String) {
            val parsed = runCatching { UUID.fromString(id) }.getOrNull()
            require(parsed != null && parsed != UUID(0L, 0L) && parsed.toString() == id)
            require(CANONICAL_ITEM_IDEMPOTENCY_KEY.matches(idempotencyKey))
        }

        private fun requireCanonicalItemRevision(expectedRevision: Long) {
            require(expectedRevision in 1 until Long.MAX_VALUE)
        }

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .callTimeout(45, TimeUnit.SECONDS)
            .retryOnConnectionFailure(true)
            .followRedirects(false)
            .followSslRedirects(false)
            .build()

        fun defaultJson(): Json = Json {
            // Canonical state is round-tripped later. A new top-level field must trigger an
            // explicit client update instead of being silently erased from the encrypted cache.
            ignoreUnknownKeys = false
            explicitNulls = false
            encodeDefaults = true
        }

        private fun Reader.readBoundedPlannerText(
            maxChars: Int = MAX_RESPONSE_CHARS,
        ): String {
            val result = StringBuilder()
            val buffer = CharArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = read(buffer)
                if (read < 0) break
                if (result.length + read > maxChars) {
                    throw PlannerApiException.InvalidResponse()
                }
                result.append(buffer, 0, read)
            }
            return result.toString()
        }
    }
}

internal fun buildSchedulePublishHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    request: SchedulePublishRequest,
): SchedulePublishHttpRequest {
    val body = OkHttpCanonicalPlannerTransport.defaultJson().encodeToString(request)
    val result = SchedulePublishHttpRequest(
        url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/schedule/publish")
            .build()
            .toString(),
        method = SCHEDULE_PUBLISH_METHOD,
        acceptHeader = SCHEDULE_PUBLISH_ACCEPT,
        contentTypeHeader = SCHEDULE_PUBLISH_CONTENT_TYPE,
        cacheControlHeader = SCHEDULE_PUBLISH_CACHE_CONTROL,
        pragmaHeader = SCHEDULE_PUBLISH_PRAGMA,
        bodyJson = body,
        bodySha256 = plannerSha256(body),
    )
    validateSchedulePublishHttpRequest(configuration, result)
    return result
}

internal fun validateSchedulePublishHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    request: SchedulePublishHttpRequest,
): SchedulePublishRequest = validateSchedulePublishHttpRequest(
    expectedBaseUrl = configuration.baseUrl.toString(),
    request = request,
)

internal fun validateSchedulePublishHttpRequest(
    expectedBaseUrl: String,
    request: SchedulePublishHttpRequest,
): SchedulePublishRequest {
    val baseUrl = expectedBaseUrl.toHttpUrlOrNull()
        ?: throw IllegalArgumentException("Invalid publication origin")
    require(baseUrl.toString() == expectedBaseUrl)
    require(baseUrl.query == null && baseUrl.fragment == null && baseUrl.username.isEmpty())
    require(baseUrl.password.isEmpty())
    require(baseUrl.isHttps || baseUrl.host in setOf("127.0.0.1", "localhost", "::1"))
    val expectedUrl = baseUrl.newBuilder()
        .addPathSegments("v1/schedule/publish")
        .build()
        .toString()
    require(request.url == expectedUrl)
    require(request.method == SCHEDULE_PUBLISH_METHOD)
    require(request.acceptHeader == SCHEDULE_PUBLISH_ACCEPT)
    require(request.contentTypeHeader == SCHEDULE_PUBLISH_CONTENT_TYPE)
    require(request.cacheControlHeader == SCHEDULE_PUBLISH_CACHE_CONTROL)
    require(request.pragmaHeader == SCHEDULE_PUBLISH_PRAGMA)
    require(
        request.bodyJson.toByteArray(StandardCharsets.UTF_8).size <=
            MAX_SCHEDULE_PUBLISH_BODY_BYTES,
    )
    require(request.bodySha256 == plannerSha256(request.bodyJson))
    val json = OkHttpCanonicalPlannerTransport.defaultJson()
    val decoded = json.decodeFromString<SchedulePublishRequest>(request.bodyJson)
    require(json.encodeToString(decoded) == request.bodyJson)
    require(UUID.fromString(decoded.idempotencyKey).toString() == decoded.idempotencyKey)
    requireScheduleInputDigest(decoded.expectedInputDigest)
    requireNotNull(runCatching { Instant.parse(decoded.schedule.asOf) }.getOrNull())
    val start = requireNotNull(
        runCatching { Instant.parse(decoded.schedule.horizonStart) }.getOrNull(),
    )
    val end = requireNotNull(
        runCatching { Instant.parse(decoded.schedule.horizonEnd) }.getOrNull(),
    )
    require(end > start)
    require(decoded.schedule.timezoneName.isNotBlank())
    return decoded
}

internal fun requireScheduleInputDigest(value: String) {
    require(SCHEDULE_INPUT_DIGEST.matches(value))
}

internal fun plannerSha256(value: String): String = "sha256:" +
    MessageDigest.getInstance("SHA-256")
        .digest(value.toByteArray(StandardCharsets.UTF_8))
        .joinToString("") { byte -> "%02x".format(byte.toInt() and 0xff) }

private const val SCHEDULE_PUBLISH_METHOD = "POST"
private const val SCHEDULE_PUBLISH_ACCEPT = "application/json"
private const val SCHEDULE_PUBLISH_CONTENT_TYPE = "application/json; charset=utf-8"
private const val SCHEDULE_PUBLISH_CACHE_CONTROL = "no-store"
private const val SCHEDULE_PUBLISH_PRAGMA = "no-cache"
/** Checked before a schedule-publication request can enter the encrypted crash journal. */
internal const val MAX_SCHEDULE_PUBLISH_BODY_BYTES = 12 * 1024 * 1024
private val SCHEDULE_INPUT_DIGEST = Regex("^sha256:[0-9a-f]{64}$")

@OptIn(ExperimentalCoroutinesApi::class)
private suspend fun Call.awaitPlannerResponse(): Response =
    suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { cancel() }
        enqueue(
            object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (continuation.isActive) continuation.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    if (continuation.isActive) {
                        continuation.resume(response) {
                                _: Throwable,
                                responseToClose: Response,
                                _: CoroutineContext,
                            ->
                            responseToClose.close()
                        }
                    } else {
                        response.close()
                    }
                }
            },
        )
    }
