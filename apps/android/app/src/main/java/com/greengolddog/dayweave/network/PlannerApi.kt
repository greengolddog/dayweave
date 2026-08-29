package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
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
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
data class RemoteCanonicalItem(
    val id: String,
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
data class SchedulePreviewRequest(
    @SerialName("as_of") val asOf: String,
    @SerialName("horizon_start") val horizonStart: String,
    @SerialName("horizon_end") val horizonEnd: String,
    @SerialName("timezone_name") val timezoneName: String,
    val availability: List<ScheduleAvailabilityRequest>,
    @SerialName("fixed_blocks") val fixedBlocks: List<JsonObject> = emptyList(),
    @SerialName("previous_assignments")
    val previousAssignments: List<PreviousScheduleAssignmentRequest> = emptyList(),
    val config: ScheduleConfigRequest = ScheduleConfigRequest(),
    @SerialName("recurrence_context") val recurrenceContext: JsonObject = JsonObject(emptyMap()),
)

@Serializable
data class RemoteRejectedScheduleItem(
    @SerialName("item_id") val itemId: String,
    val title: String,
    val reason: String,
)

@Serializable
data class RemoteScheduleBlock(
    val id: String,
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

    suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
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

    override suspend fun replaceItem(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        idempotencyKey: String,
        request: ReplaceCanonicalItemRequest,
    ): RemoteCanonicalItem {
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

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(request: Request): T {
        val response = client.newCall(request).awaitPlannerResponse()
        response.use {
            if (response.code != 200) throw response.toPlannerApiException()
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

    private fun Response.toPlannerApiException(): PlannerApiException = when (code) {
        401 -> PlannerApiException.Authentication()
        409 -> PlannerApiException.Conflict()
        400, 422 -> PlannerApiException.Validation(code)
        else -> PlannerApiException.Http(code)
    }

    companion object {
        const val MAX_DELTA_PAGE_SIZE = 50
        // One page can legitimately contain large notes plus bounded recurrence/constraint JSON.
        private const val MAX_RESPONSE_CHARS = 12 * 1024 * 1024
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()

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

        private fun Reader.readBoundedPlannerText(): String {
            val result = StringBuilder()
            val buffer = CharArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = read(buffer)
                if (read < 0) break
                if (result.length + read > MAX_RESPONSE_CHARS) {
                    throw PlannerApiException.InvalidResponse()
                }
                result.append(buffer, 0, read)
            }
            return result.toString()
        }
    }
}

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
