package com.greengolddog.dayweave.network

import com.greengolddog.dayweave.model.CanonicalBlockedReasonKind
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.requireValidStructuralMetadata
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.ZoneId
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

const val PROPOSAL_CHANGE_SET_SCHEMA_V1 = "dayweave.proposal-change-set/1"

/**
 * Exact non-secret request persisted before an apply or undo network send.
 *
 * Authorization is deliberately absent. It is attached from the currently bound durable device
 * credential only after this envelope has been revalidated against that configuration.
 */
@Serializable
data class ProposalApplicationHttpRequest(
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
        "ProposalApplicationHttpRequest(url=$url, method=$method, body=<redacted>)"
}

@Serializable
data class ProposalPreviewMember(
    @SerialName("proposal_id") val proposalId: String,
    @SerialName("expected_revision") val expectedRevision: Long,
)

@Serializable
data class ProposalPreviewRequest(val proposals: List<ProposalPreviewMember>)

@Serializable
enum class RemoteProposalOperation {
    @SerialName("create_item")
    CREATE_ITEM,

    @SerialName("replace_item")
    REPLACE_ITEM,

    @SerialName("trash_item")
    TRASH_ITEM,

    @SerialName("restore_item")
    RESTORE_ITEM,
}

@Serializable
enum class RemoteProposalItemField {
    @SerialName("is_sensitive") IS_SENSITIVE,
    @SerialName("kind") KIND,
    @SerialName("status") STATUS,
    @SerialName("title") TITLE,
    @SerialName("notes") NOTES,
    @SerialName("timezone_name") TIMEZONE_NAME,
    @SerialName("duration_kind") DURATION_KIND,
    @SerialName("duration_min_seconds") DURATION_MIN_SECONDS,
    @SerialName("duration_seconds") DURATION_SECONDS,
    @SerialName("duration_max_seconds") DURATION_MAX_SECONDS,
    @SerialName("duration_source") DURATION_SOURCE,
    @SerialName("deadline_kind") DEADLINE_KIND,
    @SerialName("deadline_at") DEADLINE_AT,
    @SerialName("deadline_date") DEADLINE_DATE,
    @SerialName("deadline_strength") DEADLINE_STRENGTH,
    @SerialName("deadline_soft_weight") DEADLINE_SOFT_WEIGHT,
    @SerialName("earliest_start_at") EARLIEST_START_AT,
    @SerialName("recurrence") RECURRENCE,
    @SerialName("flexible_constraints") FLEXIBLE_CONSTRAINTS,
    @SerialName("dependencies") DEPENDENCIES,
    @SerialName("split_policy") SPLIT_POLICY,
    @SerialName("importance") IMPORTANCE,
    @SerialName("urgency") URGENCY,
    @SerialName("parent_id") PARENT_ID,
    @SerialName("sibling_order") SIBLING_ORDER,
    @SerialName("has_own_effort") HAS_OWN_EFFORT,
    @SerialName("blocked_reason_kind") BLOCKED_REASON_KIND,
    @SerialName("blocked_by_item_id") BLOCKED_BY_ITEM_ID,
    @SerialName("blocked_reason") BLOCKED_REASON,
    @SerialName("is_executable") IS_EXECUTABLE,
    @SerialName("revision") REVISION,
    @SerialName("completed_at") COMPLETED_AT,
    @SerialName("deleted_at") DELETED_AT,
}

@Serializable
enum class RemoteProposalItemKind(val wireValue: String) {
    @SerialName("event") EVENT("event"),
    @SerialName("task") TASK("task"),
    @SerialName("habit") HABIT("habit"),
    @SerialName("routine") ROUTINE("routine"),
    @SerialName("goal") GOAL("goal"),
    @SerialName("project") PROJECT("project"),
    @SerialName("break") BREAK("break"),
}

@Serializable
enum class RemoteProposalItemStatus(val wireValue: String) {
    @SerialName("inbox") INBOX("inbox"),
    @SerialName("planned") PLANNED("planned"),
    @SerialName("scheduled") SCHEDULED("scheduled"),
    @SerialName("in_progress") IN_PROGRESS("in_progress"),
    @SerialName("paused") PAUSED("paused"),
    @SerialName("blocked") BLOCKED("blocked"),
    @SerialName("completed") COMPLETED("completed"),
    @SerialName("skipped") SKIPPED("skipped"),
    @SerialName("cancelled") CANCELLED("cancelled"),
}

/** Strict canonical-item projection used only inside an application review diff. */
@Serializable
data class RemoteProposalCanonicalItem(
    val id: String,
    @SerialName("is_sensitive") val isSensitive: Boolean,
    val kind: RemoteProposalItemKind,
    val status: RemoteProposalItemStatus,
    val title: String,
    val notes: String?,
    @SerialName("timezone_name") val timezoneName: String,
    @SerialName("duration_seconds") val durationSeconds: Long?,
    @SerialName("duration_kind") val durationKind: CanonicalDurationKind? = null,
    @SerialName("duration_min_seconds") val durationMinSeconds: Long? = null,
    @SerialName("duration_max_seconds") val durationMaxSeconds: Long? = null,
    @SerialName("duration_source") val durationSource: CanonicalDurationSource? = null,
    @SerialName("deadline_at") val deadlineAt: String?,
    @SerialName("deadline_kind") val deadlineKind: CanonicalDeadlineKind? = null,
    @SerialName("deadline_date") val deadlineDate: String? = null,
    @SerialName("deadline_strength") val deadlineStrength: CanonicalDeadlineStrength? = null,
    @SerialName("deadline_soft_weight") val deadlineSoftWeight: Long? = null,
    @SerialName("earliest_start_at") val earliestStartAt: String?,
    val recurrence: JsonElement?,
    @SerialName("flexible_constraints") val flexibleConstraints: JsonObject,
    @SerialName("split_policy") val splitPolicy: JsonObject,
    val importance: Int,
    val urgency: Int,
    @SerialName("parent_id") val parentId: String?,
    @SerialName("sibling_order") val siblingOrder: Long,
    @SerialName("has_own_effort") val hasOwnEffort: Boolean? = null,
    @SerialName("blocked_reason_kind")
    val blockedReasonKind: CanonicalBlockedReasonKind? = null,
    @SerialName("blocked_by_item_id") val blockedByItemId: String? = null,
    @SerialName("blocked_reason") val blockedReason: String? = null,
    @SerialName("is_executable") val isExecutable: Boolean,
    val revision: Long,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
    @SerialName("completed_at") val completedAt: String?,
    @SerialName("deleted_at") val deletedAt: String?,
)

@Serializable
data class RemoteProposalItemDiff(
    @SerialName("command_id") val commandId: String,
    val operation: RemoteProposalOperation,
    @SerialName("item_id") val itemId: String,
    @SerialName("changed_fields") val changedFields: List<RemoteProposalItemField>,
    val before: RemoteProposalCanonicalItem?,
    val after: RemoteProposalCanonicalItem?,
)

@Serializable
enum class RemoteProposalImplicitChangeReason {
    @SerialName("hierarchy_refresh") HIERARCHY_REFRESH,
}

@Serializable
data class RemoteProposalImplicitItemDiff(
    @SerialName("item_id") val itemId: String,
    val reason: RemoteProposalImplicitChangeReason,
    @SerialName("changed_fields") val changedFields: List<RemoteProposalItemField>,
    val before: RemoteProposalCanonicalItem,
    val after: RemoteProposalCanonicalItem,
)

@Serializable
enum class RemoteProposalRiskLevel {
    @SerialName("low") LOW,
    @SerialName("medium") MEDIUM,
    @SerialName("high") HIGH,
    @SerialName("critical") CRITICAL,
}

@Serializable
enum class RemoteProposalRiskCode {
    @SerialName("creates_item") CREATES_ITEM,
    @SerialName("replaces_item") REPLACES_ITEM,
    @SerialName("trashes_item") TRASHES_ITEM,
    @SerialName("restores_item") RESTORES_ITEM,
    @SerialName("changes_deadline") CHANGES_DEADLINE,
    @SerialName("relaxes_deadline") RELAXES_DEADLINE,
    @SerialName("changes_hierarchy") CHANGES_HIERARCHY,
    @SerialName("changes_dependencies") CHANGES_DEPENDENCIES,
    @SerialName("changes_sensitivity") CHANGES_SENSITIVITY,
    @SerialName("changes_recurrence") CHANGES_RECURRENCE,
    @SerialName("changes_execution_state") CHANGES_EXECUTION_STATE,
    @SerialName("sensitive_content") SENSITIVE_CONTENT,
    @SerialName("bulk_change") BULK_CHANGE,
}

@Serializable
data class RemoteProposalRisk(
    val code: RemoteProposalRiskCode,
    val level: RemoteProposalRiskLevel,
    @SerialName("command_id") val commandId: String?,
    @SerialName("item_id") val itemId: String?,
    @SerialName("requires_explicit_approval") val requiresExplicitApproval: Boolean,
    val summary: String,
)

@Serializable
enum class RemoteProposalConflictCode {
    @SerialName("proposal_not_pending") PROPOSAL_NOT_PENDING,
    @SerialName("proposal_expired") PROPOSAL_EXPIRED,
    @SerialName("proposal_revision_mismatch") PROPOSAL_REVISION_MISMATCH,
    @SerialName("item_already_exists") ITEM_ALREADY_EXISTS,
    @SerialName("item_not_found") ITEM_NOT_FOUND,
    @SerialName("item_revision_mismatch") ITEM_REVISION_MISMATCH,
    @SerialName("parent_not_found") PARENT_NOT_FOUND,
    @SerialName("hierarchy_cycle") HIERARCHY_CYCLE,
    @SerialName("dependency_not_found") DEPENDENCY_NOT_FOUND,
    @SerialName("dependency_cycle") DEPENDENCY_CYCLE,
    @SerialName("invalid_parent_state") INVALID_PARENT_STATE,
    @SerialName("non_leaf_executable") NON_LEAF_EXECUTABLE,
    @SerialName("has_children") HAS_CHILDREN,
    @SerialName("deleted_parent") DELETED_PARENT,
    @SerialName("invalid_item") INVALID_ITEM,
    @SerialName("provider_managed_item") PROVIDER_MANAGED_ITEM,
    @SerialName("preview_expired") PREVIEW_EXPIRED,
    @SerialName("preview_mismatch") PREVIEW_MISMATCH,
    @SerialName("preview_not_applicable") PREVIEW_NOT_APPLICABLE,
    @SerialName("already_applied") ALREADY_APPLIED,
    @SerialName("undo_expired") UNDO_EXPIRED,
    @SerialName("undo_diverged") UNDO_DIVERGED,
}

@Serializable
data class RemoteProposalConflict(
    val code: RemoteProposalConflictCode,
    @SerialName("command_id") val commandId: String?,
    @SerialName("item_id") val itemId: String?,
    @SerialName("expected_revision") val expectedRevision: Long?,
    @SerialName("actual_revision") val actualRevision: Long?,
    val summary: String,
)

@Serializable
data class RemoteProposalApplicationPreview(
    @SerialName("preview_id") val previewId: String,
    val proposals: List<ProposalPreviewMember>,
    @SerialName("change_set_schema") val changeSetSchema: String,
    @SerialName("command_ids") val commandIds: List<String>,
    @SerialName("review_hash") val reviewHash: String,
    @SerialName("expires_at") val expiresAt: String,
    @SerialName("can_apply") val canApply: Boolean,
    @SerialName("maximum_risk") val maximumRisk: RemoteProposalRiskLevel,
    @SerialName("requires_explicit_approval") val requiresExplicitApproval: Boolean,
    val diffs: List<RemoteProposalItemDiff>,
    @SerialName("implicit_diffs") val implicitDiffs: List<RemoteProposalImplicitItemDiff>,
    val risks: List<RemoteProposalRisk>,
    val conflicts: List<RemoteProposalConflict>,
)

@Serializable
enum class RemoteProposalApplicationStatus {
    @SerialName("applied") APPLIED,
    @SerialName("undone") UNDONE,
}

@Serializable
data class RemoteProposalAppliedMember(
    @SerialName("proposal_id") val proposalId: String,
    @SerialName("applied_revision") val appliedRevision: Long,
)

@Serializable
data class RemoteProposalApplicationReceipt(
    @SerialName("application_id") val applicationId: String,
    val proposals: List<RemoteProposalAppliedMember>,
    @SerialName("application_revision") val applicationRevision: Long,
    val status: RemoteProposalApplicationStatus,
    @SerialName("command_ids") val commandIds: List<String>,
    @SerialName("affected_item_ids") val affectedItemIds: List<String>,
    @SerialName("applied_at") val appliedAt: String,
    @SerialName("undo_expires_at") val undoExpiresAt: String,
    @SerialName("undone_at") val undoneAt: String?,
)

@Serializable
data class RemoteProposalApplyResponse(
    val application: RemoteProposalApplicationReceipt,
    val replayed: Boolean,
)

@Serializable
data class RemoteProposalUndoResponse(
    val application: RemoteProposalApplicationReceipt,
    val replayed: Boolean,
)

@Serializable
private data class ProposalApplyRequest(
    @SerialName("expected_review_hash") val expectedReviewHash: String,
)

@Serializable
private data class ProposalUndoRequest(
    @SerialName("expected_application_revision") val expectedApplicationRevision: Long,
)

sealed class ProposalApplicationApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : ProposalApplicationApiException(
        "The DayWeave API rejected the device credential",
    )

    class Authorization : ProposalApplicationApiException(
        "The device credential cannot use proposal applications",
    )

    class NotFound : ProposalApplicationApiException(
        "The proposal application resource was not found",
    )

    class Conflict(
        val conflictCode: RemoteProposalConflictCode? = null,
    ) : ProposalApplicationApiException(
        "The proposal application review or canonical state changed",
    )

    class Validation(val statusCode: Int) : ProposalApplicationApiException(
        "The DayWeave API rejected a proposal application request with HTTP $statusCode",
    )

    class Http(val statusCode: Int) : ProposalApplicationApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidRequest(cause: Throwable? = null) : ProposalApplicationApiException(
        "The persisted proposal application request is invalid",
        cause,
    )

    class InvalidResponse(cause: Throwable? = null) : ProposalApplicationApiException(
        "The DayWeave API returned an invalid proposal application response",
        cause,
    )
}

interface ProposalApplicationsTransport {
    suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: ProposalPreviewRequest,
    ): RemoteProposalApplicationPreview

    /** Sends the exact validated URL, headers, and body bytes from the durable journal. */
    suspend fun apply(
        configuration: AuthenticatedApiConfiguration,
        previewId: String,
        expectedReviewHash: String,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalApplyResponse

    suspend fun getById(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
    ): RemoteProposalApplicationReceipt

    suspend fun getByProposal(
        configuration: AuthenticatedApiConfiguration,
        proposalId: String,
    ): RemoteProposalApplicationReceipt

    /** Sends the exact validated URL, headers, and body bytes from the durable journal. */
    suspend fun undo(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
        expectedApplicationRevision: Long,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalUndoResponse
}

class OkHttpProposalApplicationsTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val json: Json = proposalApplicationJson(),
    private val maximumResponseBytes: Int = MAX_RESPONSE_BYTES,
) : ProposalApplicationsTransport {
    init {
        require(maximumResponseBytes in 1..MAX_RESPONSE_BYTES)
    }

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: ProposalPreviewRequest,
    ): RemoteProposalApplicationPreview {
        invalidRequest { validatePreviewRequest(request) }
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions/application-previews")
            .build()
        val requestJson = invalidRequest { json.encodeToString(request) }
        val httpRequest = requestBuilder(configuration, url.toString())
            .post(requestJson.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        val preview = execute<RemoteProposalApplicationPreview>(
            request = httpRequest,
            expectedStatus = 201,
            validateBody = ::requireAtomicProposalStructuralMetadata,
        )
        validateResponse { validatePreview(preview) }
        if (preview.proposals != request.proposals) {
            throw ProposalApplicationApiException.InvalidResponse()
        }
        return preview
    }

    override suspend fun apply(
        configuration: AuthenticatedApiConfiguration,
        previewId: String,
        expectedReviewHash: String,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalApplyResponse {
        invalidRequest {
            validateIdempotencyKey(idempotencyKey)
            validateProposalApplyHttpRequest(
                configuration,
                request,
                previewId,
                expectedReviewHash,
            )
        }
        val response = execute<RemoteProposalApplyResponse>(
            exactMutationRequest(configuration, request, idempotencyKey),
            expectedStatus = 200,
        )
        validateResponse { validateReceipt(response.application) }
        return response
    }

    override suspend fun getById(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
    ): RemoteProposalApplicationReceipt {
        invalidRequest { requireCanonicalUuid(applicationId) }
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions/applications")
            .addPathSegment(applicationId)
            .build()
        val receipt = execute<RemoteProposalApplicationReceipt>(
            requestBuilder(configuration, url.toString()).get().build(),
            expectedStatus = 200,
        )
        validateResponse {
            validateReceipt(receipt)
            require(receipt.applicationId == applicationId)
        }
        return receipt
    }

    override suspend fun getByProposal(
        configuration: AuthenticatedApiConfiguration,
        proposalId: String,
    ): RemoteProposalApplicationReceipt {
        invalidRequest { requireCanonicalUuid(proposalId) }
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions")
            .addPathSegment(proposalId)
            .addPathSegment("application")
            .build()
        val receipt = execute<RemoteProposalApplicationReceipt>(
            requestBuilder(configuration, url.toString()).get().build(),
            expectedStatus = 200,
        )
        validateResponse {
            validateReceipt(receipt)
            require(receipt.proposals.any { member -> member.proposalId == proposalId })
        }
        return receipt
    }

    override suspend fun undo(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
        expectedApplicationRevision: Long,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalUndoResponse {
        invalidRequest {
            validateIdempotencyKey(idempotencyKey)
            validateProposalUndoHttpRequest(
                configuration,
                request,
                applicationId,
                expectedApplicationRevision,
            )
        }
        val response = execute<RemoteProposalUndoResponse>(
            exactMutationRequest(configuration, request, idempotencyKey),
            expectedStatus = 200,
        )
        validateResponse {
            validateReceipt(response.application)
            require(response.application.applicationId == applicationId)
        }
        return response
    }

    private fun exactMutationRequest(
        configuration: AuthenticatedApiConfiguration,
        request: ProposalApplicationHttpRequest,
        idempotencyKey: String,
    ): Request = Request.Builder()
        .url(request.url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", request.acceptHeader)
        .header("Authorization", "Bearer ${configuration.bearerToken}")
        .header("Cache-Control", request.cacheControlHeader)
        .header("Pragma", request.pragmaHeader)
        .header("Content-Type", request.contentTypeHeader)
        .header("Idempotency-Key", idempotencyKey)
        .post(request.bodyJson.toRequestBody(request.contentTypeHeader.toMediaType()))
        .build()

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", PROPOSAL_APPLICATION_ACCEPT)
        .header("Authorization", "Bearer ${configuration.bearerToken}")
        .header("Cache-Control", PROPOSAL_APPLICATION_CACHE_CONTROL)
        .header("Pragma", PROPOSAL_APPLICATION_PRAGMA)

    private suspend inline fun <reified T> execute(
        request: Request,
        expectedStatus: Int,
        noinline validateBody: ((String) -> Unit)? = null,
    ): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw ProposalApplicationApiException.InvalidRequest()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != expectedStatus) throw response.toProposalApplicationException()
            if (!response.hasExactProposalApplicationJsonMediaType()) {
                throw ProposalApplicationApiException.InvalidResponse()
            }
            val responseText = response.readBoundedProposalApplicationText()
            try {
                validateBody?.invoke(responseText)
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw ProposalApplicationApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw ProposalApplicationApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toProposalApplicationException(): ProposalApplicationApiException {
        val expectedCode = when (code) {
            400 -> "invalid_json"
            401 -> "unauthorized"
            403 -> "forbidden"
            404 -> "not_found"
            409 -> "conflict"
            413 -> "payload_too_large"
            422 -> "validation_failed"
            500 -> "internal_error"
            502 -> "bad_gateway"
            503 -> "service_unavailable"
            else -> return ProposalApplicationApiException.Http(code)
        }
        if (
            !hasStrictProposalApplicationNoStoreHeaders() ||
            !hasExactProposalApplicationJsonMediaType() ||
            code == 401 && headers.values("WWW-Authenticate") !=
            listOf("Bearer realm=\"dayweave\"")
        ) {
            return ProposalApplicationApiException.InvalidResponse()
        }
        val responseText = try {
            readBoundedProposalApplicationText()
        } catch (error: ProposalApplicationApiException.InvalidResponse) {
            return error
        }
        val errorObject = try {
            val outer = json.parseToJsonElement(responseText) as? JsonObject
                ?: return ProposalApplicationApiException.InvalidResponse()
            if (outer.keys != setOf("error")) {
                return ProposalApplicationApiException.InvalidResponse()
            }
            outer["error"] as? JsonObject
                ?: return ProposalApplicationApiException.InvalidResponse()
        } catch (error: SerializationException) {
            return ProposalApplicationApiException.InvalidResponse(error)
        } catch (error: IllegalArgumentException) {
            return ProposalApplicationApiException.InvalidResponse(error)
        }
        val allowedKeys = if (code == 409) {
            setOf("code", "message", "details")
        } else {
            setOf("code", "message")
        }
        if (!errorObject.keys.all { key -> key in allowedKeys }) {
            return ProposalApplicationApiException.InvalidResponse()
        }
        if (errorObject.keys.intersect(setOf("code", "message")) != setOf("code", "message")) {
            return ProposalApplicationApiException.InvalidResponse()
        }
        val remoteCode = (errorObject["code"] as? JsonPrimitive)
            ?.takeIf { value -> value.isString }
            ?.content
        val message = (errorObject["message"] as? JsonPrimitive)
            ?.takeIf { value -> value.isString }
            ?.content
        if (
            remoteCode != expectedCode ||
            message == null ||
            message.isBlank() ||
            message.length > MAX_ERROR_TEXT_CHARS ||
            message.any(Char::isISOControl)
        ) {
            return ProposalApplicationApiException.InvalidResponse()
        }
        if (code != 409 && "details" in errorObject) {
            return ProposalApplicationApiException.InvalidResponse()
        }
        return when (code) {
            401 -> ProposalApplicationApiException.Authentication()
            403 -> ProposalApplicationApiException.Authorization()
            404 -> ProposalApplicationApiException.NotFound()
            409 -> decodeConflict(errorObject)
            400, 413, 422 -> ProposalApplicationApiException.Validation(code)
            else -> ProposalApplicationApiException.Http(code)
        }
    }

    private fun decodeConflict(errorObject: JsonObject): ProposalApplicationApiException {
        val detailsElement = errorObject["details"]
            ?: return ProposalApplicationApiException.Conflict()
        val details = detailsElement as? JsonObject
            ?: return ProposalApplicationApiException.InvalidResponse()
        if (details.keys == setOf("conflict_code")) {
            val conflictCode = try {
                json.decodeFromJsonElement<RemoteProposalConflictCode>(
                    requireNotNull(details["conflict_code"]),
                )
            } catch (error: SerializationException) {
                return ProposalApplicationApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                return ProposalApplicationApiException.InvalidResponse(error)
            }
            return ProposalApplicationApiException.Conflict(conflictCode)
        }
        if (details.keys == setOf("expected_revision", "actual_revision")) {
            val expected = (details["expected_revision"] as? JsonPrimitive)?.longOrNull
            val actual = (details["actual_revision"] as? JsonPrimitive)?.longOrNull
            if (expected != null && expected > 0 && actual != null && actual > 0) {
                return ProposalApplicationApiException.Conflict()
            }
        }
        return ProposalApplicationApiException.InvalidResponse()
    }

    private fun Response.hasStrictProposalApplicationNoStoreHeaders(): Boolean {
        val directives = headers.values("Cache-Control")
            .flatMap { value -> value.split(',', limit = Int.MAX_VALUE) }
            .map { directive -> directive.trim().lowercase() }
        return directives.size == 2 &&
            directives.toSet() == setOf("no-store", "max-age=0") &&
            headers.values("Pragma").let { values ->
                values.size == 1 && values.single().trim().equals("no-cache", ignoreCase = true)
            }
    }

    private fun Response.hasExactProposalApplicationJsonMediaType(): Boolean {
        val values = headers.values("Content-Type")
        if (values.size != 1) return false
        val components = values.single().split(';', limit = Int.MAX_VALUE)
        if (!components.firstOrNull().orEmpty().trim().equals("application/json", true)) {
            return false
        }
        return when (components.size) {
            1 -> true
            2 -> components[1].trim().split('=', limit = 2).let { pair ->
                pair.size == 2 &&
                    pair[0].trim().equals("charset", ignoreCase = true) &&
                    pair[1].trim().equals("utf-8", ignoreCase = true)
            }
            else -> false
        }
    }

    private fun Response.readBoundedProposalApplicationText(): String {
        val declaredLength = body.contentLength()
        if (declaredLength > maximumResponseBytes) {
            throw ProposalApplicationApiException.InvalidResponse()
        }
        val output = ByteArrayOutputStream(
            declaredLength.coerceIn(0, maximumResponseBytes.toLong()).toInt(),
        )
        body.byteStream().use { input ->
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            var total = 0
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                total += read
                if (total > maximumResponseBytes) {
                    throw ProposalApplicationApiException.InvalidResponse()
                }
                output.write(buffer, 0, read)
            }
        }
        return try {
            StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(output.toByteArray()))
                .toString()
        } catch (error: Exception) {
            throw ProposalApplicationApiException.InvalidResponse(error)
        }
    }

    private companion object {
        // A review may include before and after values for many bounded canonical item payloads.
        const val MAX_RESPONSE_BYTES = 48 * 1024 * 1024
        const val MAX_ERROR_TEXT_CHARS = 512
        val JSON_MEDIA_TYPE = PROPOSAL_APPLICATION_CONTENT_TYPE.toMediaType()
    }
}

fun prepareProposalApplyHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    previewId: String,
    expectedReviewHash: String,
): ProposalApplicationHttpRequest {
    requireCanonicalUuid(previewId)
    requireReviewHash(expectedReviewHash)
    val body = proposalApplicationJson().encodeToString(ProposalApplyRequest(expectedReviewHash))
    val result = ProposalApplicationHttpRequest(
        url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions/application-previews")
            .addPathSegment(previewId)
            .addPathSegment("apply")
            .build()
            .toString(),
        method = PROPOSAL_APPLICATION_METHOD,
        acceptHeader = PROPOSAL_APPLICATION_ACCEPT,
        contentTypeHeader = PROPOSAL_APPLICATION_CONTENT_TYPE,
        cacheControlHeader = PROPOSAL_APPLICATION_CACHE_CONTROL,
        pragmaHeader = PROPOSAL_APPLICATION_PRAGMA,
        bodyJson = body,
        bodySha256 = plannerSha256(body),
    )
    validateProposalApplyHttpRequest(configuration, result, previewId, expectedReviewHash)
    return result
}

fun prepareProposalUndoHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    applicationId: String,
    expectedApplicationRevision: Long,
): ProposalApplicationHttpRequest {
    requireCanonicalUuid(applicationId)
    require(expectedApplicationRevision > 0)
    val body = proposalApplicationJson().encodeToString(
        ProposalUndoRequest(expectedApplicationRevision),
    )
    val result = ProposalApplicationHttpRequest(
        url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions/applications")
            .addPathSegment(applicationId)
            .addPathSegment("undo")
            .build()
            .toString(),
        method = PROPOSAL_APPLICATION_METHOD,
        acceptHeader = PROPOSAL_APPLICATION_ACCEPT,
        contentTypeHeader = PROPOSAL_APPLICATION_CONTENT_TYPE,
        cacheControlHeader = PROPOSAL_APPLICATION_CACHE_CONTROL,
        pragmaHeader = PROPOSAL_APPLICATION_PRAGMA,
        bodyJson = body,
        bodySha256 = plannerSha256(body),
    )
    validateProposalUndoHttpRequest(
        configuration,
        result,
        applicationId,
        expectedApplicationRevision,
    )
    return result
}

internal fun validateProposalApplyHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    request: ProposalApplicationHttpRequest,
    previewId: String,
    expectedReviewHash: String,
): Unit = validateProposalApplyHttpRequest(
    expectedBaseUrl = configuration.baseUrl.toString(),
    request = request,
    previewId = previewId,
    expectedReviewHash = expectedReviewHash,
)

internal fun validateProposalApplyHttpRequest(
    expectedBaseUrl: String,
    request: ProposalApplicationHttpRequest,
    previewId: String,
    expectedReviewHash: String,
) {
    requireCanonicalUuid(previewId)
    requireReviewHash(expectedReviewHash)
    val expectedUrl = proposalApplicationBaseUrl(expectedBaseUrl).newBuilder()
        .addPathSegments("v1/suggestions/application-previews")
        .addPathSegment(previewId)
        .addPathSegment("apply")
        .build()
        .toString()
    validateProposalApplicationEnvelope(request, expectedUrl)
    val decoded = proposalApplicationJson().decodeFromString<ProposalApplyRequest>(request.bodyJson)
    require(decoded == ProposalApplyRequest(expectedReviewHash))
    require(proposalApplicationJson().encodeToString(decoded) == request.bodyJson)
}

internal fun validateProposalUndoHttpRequest(
    configuration: AuthenticatedApiConfiguration,
    request: ProposalApplicationHttpRequest,
    applicationId: String,
    expectedApplicationRevision: Long,
): Unit = validateProposalUndoHttpRequest(
    expectedBaseUrl = configuration.baseUrl.toString(),
    request = request,
    applicationId = applicationId,
    expectedApplicationRevision = expectedApplicationRevision,
)

internal fun validateProposalUndoHttpRequest(
    expectedBaseUrl: String,
    request: ProposalApplicationHttpRequest,
    applicationId: String,
    expectedApplicationRevision: Long,
) {
    requireCanonicalUuid(applicationId)
    require(expectedApplicationRevision > 0)
    val expectedUrl = proposalApplicationBaseUrl(expectedBaseUrl).newBuilder()
        .addPathSegments("v1/suggestions/applications")
        .addPathSegment(applicationId)
        .addPathSegment("undo")
        .build()
        .toString()
    validateProposalApplicationEnvelope(request, expectedUrl)
    val decoded = proposalApplicationJson().decodeFromString<ProposalUndoRequest>(request.bodyJson)
    require(decoded == ProposalUndoRequest(expectedApplicationRevision))
    require(proposalApplicationJson().encodeToString(decoded) == request.bodyJson)
}

private fun validateProposalApplicationEnvelope(
    request: ProposalApplicationHttpRequest,
    expectedUrl: String,
) {
    require(request.url == expectedUrl)
    require(request.method == PROPOSAL_APPLICATION_METHOD)
    require(request.acceptHeader == PROPOSAL_APPLICATION_ACCEPT)
    require(request.contentTypeHeader == PROPOSAL_APPLICATION_CONTENT_TYPE)
    require(request.cacheControlHeader == PROPOSAL_APPLICATION_CACHE_CONTROL)
    require(request.pragmaHeader == PROPOSAL_APPLICATION_PRAGMA)
    require(request.bodyJson.toByteArray(StandardCharsets.UTF_8).size in 2..MAX_MUTATION_BODY_BYTES)
    require(request.bodySha256 == plannerSha256(request.bodyJson))
}

private fun proposalApplicationBaseUrl(expectedBaseUrl: String): okhttp3.HttpUrl {
    val baseUrl = expectedBaseUrl.toHttpUrlOrNull()
        ?: throw IllegalArgumentException("Invalid proposal application origin")
    require(baseUrl.toString() == expectedBaseUrl)
    require(baseUrl.query == null && baseUrl.fragment == null)
    require(baseUrl.username.isEmpty() && baseUrl.password.isEmpty())
    require(baseUrl.isHttps || baseUrl.host in setOf("127.0.0.1", "localhost", "::1"))
    return baseUrl
}

private fun proposalApplicationJson(): Json = Json {
    ignoreUnknownKeys = false
    // Nullable before/after, identifiers, timestamps, and conflict fields remain required keys.
    explicitNulls = true
    encodeDefaults = true
}

private fun validatePreviewRequest(request: ProposalPreviewRequest) {
    require(request.proposals.size in 1..MAX_PROPOSALS_PER_PREVIEW)
    require(request.proposals.map { member -> member.proposalId }.toSet().size == request.proposals.size)
    request.proposals.forEach { member ->
        requireCanonicalUuid(member.proposalId)
        require(member.expectedRevision > 0)
    }
}

private fun requireAtomicProposalStructuralMetadata(responseText: String) {
    val root = proposalApplicationJson().parseToJsonElement(responseText) as? JsonObject
        ?: throw SerializationException("Proposal preview must be an object")

    fun requireItem(element: JsonElement?) {
        if (element == null || element is kotlinx.serialization.json.JsonNull) return
        val item = element as? JsonObject
            ?: throw SerializationException("Proposal canonical item must be an object")
        val present = item.keys.intersect(PROPOSAL_STRUCTURAL_WIRE_KEYS)
        if (present.isNotEmpty() && present.size != PROPOSAL_STRUCTURAL_WIRE_KEYS.size) {
            throw SerializationException("Proposal structural metadata must be emitted atomically")
        }
        if (
            present.isNotEmpty() && listOf(
                "duration_kind",
                "deadline_kind",
                "has_own_effort",
            ).any { key -> item[key] is kotlinx.serialization.json.JsonNull }
        ) {
            throw SerializationException("Proposal structural discriminators cannot be null")
        }
    }

    val diffs = root["diffs"] as? JsonArray
        ?: throw SerializationException("Proposal diffs must be an array")
    diffs.forEach { element ->
        val diff = element as? JsonObject
            ?: throw SerializationException("Proposal diff must be an object")
        requireItem(diff["before"])
        requireItem(diff["after"])
    }
    val implicitDiffs = root["implicit_diffs"] as? JsonArray
        ?: throw SerializationException("Proposal implicit diffs must be an array")
    implicitDiffs.forEach { element ->
        val diff = element as? JsonObject
            ?: throw SerializationException("Proposal implicit diff must be an object")
        requireItem(diff["before"])
        requireItem(diff["after"])
    }
}

private fun validatePreview(preview: RemoteProposalApplicationPreview) {
    requireCanonicalUuid(preview.previewId)
    validatePreviewRequest(ProposalPreviewRequest(preview.proposals))
    require(preview.changeSetSchema == PROPOSAL_CHANGE_SET_SCHEMA_V1)
    require(preview.commandIds.size in 1..MAX_PROPOSAL_COMMANDS)
    require(preview.commandIds.toSet().size == preview.commandIds.size)
    preview.commandIds.forEach(::requireCanonicalUuid)
    requireReviewHash(preview.reviewHash)
    requireInstant(preview.expiresAt)
    require(preview.canApply == preview.conflicts.isEmpty())

    val commandIds = preview.commandIds.toSet()
    val directItemIds = mutableSetOf<String>()
    val diffCommandIds = mutableSetOf<String>()
    preview.diffs.forEach { diff ->
        requireCanonicalUuid(diff.commandId)
        require(diff.commandId in commandIds)
        require(diffCommandIds.add(diff.commandId))
        requireCanonicalUuid(diff.itemId)
        require(directItemIds.add(diff.itemId))
        require(diff.changedFields.isNotEmpty())
        require(diff.changedFields.toSet().size == diff.changedFields.size)
        diff.before?.let(::validateCanonicalItem)
        diff.after?.let(::validateCanonicalItem)
        require(diff.before?.id == null || diff.before.id == diff.itemId)
        require(diff.after?.id == null || diff.after.id == diff.itemId)
        require(diff.changedFields == materialChangedFields(diff.before, diff.after))
        when (diff.operation) {
            RemoteProposalOperation.CREATE_ITEM -> require(diff.before == null && diff.after != null)
            RemoteProposalOperation.REPLACE_ITEM,
            RemoteProposalOperation.TRASH_ITEM,
            RemoteProposalOperation.RESTORE_ITEM,
            -> require(diff.before != null && diff.after != null)
        }
    }
    if (preview.canApply) {
        require(diffCommandIds == commandIds)
    }

    val implicitItemIds = mutableSetOf<String>()
    preview.implicitDiffs.forEach { diff ->
        requireCanonicalUuid(diff.itemId)
        require(implicitItemIds.add(diff.itemId))
        require(diff.itemId !in directItemIds)
        require(diff.changedFields.isNotEmpty())
        require(diff.changedFields.toSet().size == diff.changedFields.size)
        validateCanonicalItem(diff.before)
        validateCanonicalItem(diff.after)
        require(diff.before.id == diff.itemId && diff.after.id == diff.itemId)
        require(diff.changedFields == materialChangedFields(diff.before, diff.after))
    }

    preview.risks.forEach { risk ->
        risk.commandId?.let { commandId ->
            requireCanonicalUuid(commandId)
            require(commandId in commandIds)
        }
        risk.itemId?.let(::requireCanonicalUuid)
        requireBoundedSummary(risk.summary)
    }
    val expectedMaximum = preview.risks.maxOfOrNull { risk -> risk.level }
        ?: RemoteProposalRiskLevel.LOW
    require(preview.maximumRisk == expectedMaximum)
    require(
        preview.requiresExplicitApproval ==
            preview.risks.any { risk -> risk.requiresExplicitApproval },
    )

    preview.conflicts.forEach { conflict ->
        conflict.commandId?.let { commandId ->
            requireCanonicalUuid(commandId)
            require(commandId in commandIds)
        }
        conflict.itemId?.let(::requireCanonicalUuid)
        conflict.expectedRevision?.let { revision -> require(revision > 0) }
        conflict.actualRevision?.let { revision -> require(revision > 0) }
        requireBoundedSummary(conflict.summary)
    }
}

private fun materialChangedFields(
    before: RemoteProposalCanonicalItem?,
    after: RemoteProposalCanonicalItem?,
): List<RemoteProposalItemField> {
    if (before == null) {
        return if (after?.hasStructuralWireShape == true) {
            MATERIAL_PROPOSAL_ITEM_FIELDS
        } else {
            LEGACY_MATERIAL_PROPOSAL_ITEM_FIELDS
        }
    }
    if (after == null) return listOf(RemoteProposalItemField.DELETED_AT)
    return buildList {
        fun changed(
            field: RemoteProposalItemField,
            beforeValue: Any?,
            afterValue: Any?,
        ) {
            if (beforeValue != afterValue) add(field)
        }
        changed(RemoteProposalItemField.IS_SENSITIVE, before.isSensitive, after.isSensitive)
        changed(RemoteProposalItemField.KIND, before.kind, after.kind)
        changed(RemoteProposalItemField.STATUS, before.status, after.status)
        changed(RemoteProposalItemField.TITLE, before.title, after.title)
        changed(RemoteProposalItemField.NOTES, before.notes, after.notes)
        changed(RemoteProposalItemField.TIMEZONE_NAME, before.timezoneName, after.timezoneName)
        changed(RemoteProposalItemField.DURATION_KIND, before.durationKind, after.durationKind)
        changed(
            RemoteProposalItemField.DURATION_SECONDS,
            before.durationSeconds,
            after.durationSeconds,
        )
        changed(
            RemoteProposalItemField.DURATION_MIN_SECONDS,
            before.durationMinSeconds,
            after.durationMinSeconds,
        )
        changed(
            RemoteProposalItemField.DURATION_MAX_SECONDS,
            before.durationMaxSeconds,
            after.durationMaxSeconds,
        )
        changed(
            RemoteProposalItemField.DURATION_SOURCE,
            before.durationSource,
            after.durationSource,
        )
        changed(RemoteProposalItemField.DEADLINE_KIND, before.deadlineKind, after.deadlineKind)
        changed(RemoteProposalItemField.DEADLINE_DATE, before.deadlineDate, after.deadlineDate)
        changed(RemoteProposalItemField.DEADLINE_AT, before.deadlineAt, after.deadlineAt)
        changed(
            RemoteProposalItemField.DEADLINE_STRENGTH,
            before.deadlineStrength,
            after.deadlineStrength,
        )
        changed(
            RemoteProposalItemField.DEADLINE_SOFT_WEIGHT,
            before.deadlineSoftWeight,
            after.deadlineSoftWeight,
        )
        changed(
            RemoteProposalItemField.EARLIEST_START_AT,
            before.earliestStartAt,
            after.earliestStartAt,
        )
        changed(RemoteProposalItemField.RECURRENCE, before.recurrence, after.recurrence)
        changed(
            RemoteProposalItemField.FLEXIBLE_CONSTRAINTS,
            before.flexibleConstraints.withoutDependencies(),
            after.flexibleConstraints.withoutDependencies(),
        )
        changed(
            RemoteProposalItemField.DEPENDENCIES,
            before.flexibleConstraints.dependencies(),
            after.flexibleConstraints.dependencies(),
        )
        changed(RemoteProposalItemField.HAS_OWN_EFFORT, before.hasOwnEffort, after.hasOwnEffort)
        changed(RemoteProposalItemField.SPLIT_POLICY, before.splitPolicy, after.splitPolicy)
        changed(RemoteProposalItemField.IMPORTANCE, before.importance, after.importance)
        changed(RemoteProposalItemField.URGENCY, before.urgency, after.urgency)
        changed(RemoteProposalItemField.PARENT_ID, before.parentId, after.parentId)
        changed(RemoteProposalItemField.SIBLING_ORDER, before.siblingOrder, after.siblingOrder)
        changed(
            RemoteProposalItemField.BLOCKED_REASON_KIND,
            before.blockedReasonKind,
            after.blockedReasonKind,
        )
        changed(
            RemoteProposalItemField.BLOCKED_BY_ITEM_ID,
            before.blockedByItemId,
            after.blockedByItemId,
        )
        changed(RemoteProposalItemField.BLOCKED_REASON, before.blockedReason, after.blockedReason)
        changed(RemoteProposalItemField.IS_EXECUTABLE, before.isExecutable, after.isExecutable)
        changed(RemoteProposalItemField.REVISION, before.revision, after.revision)
        changed(RemoteProposalItemField.COMPLETED_AT, before.completedAt, after.completedAt)
        changed(RemoteProposalItemField.DELETED_AT, before.deletedAt, after.deletedAt)
    }
}

private fun JsonObject.dependencies(): JsonArray =
    (get("constraints") as? JsonObject)?.get("dependencies") as? JsonArray ?: JsonArray(emptyList())

private fun JsonObject.withoutDependencies(): JsonObject {
    val constraints = get("constraints") as? JsonObject ?: return this
    if ("dependencies" !in constraints) return this
    val remainingConstraints = JsonObject(constraints - "dependencies")
    return if (remainingConstraints.isEmpty()) {
        JsonObject(this - "constraints")
    } else {
        JsonObject(this + ("constraints" to remainingConstraints))
    }
}

private val RemoteProposalCanonicalItem.hasStructuralWireShape: Boolean
    get() = durationKind != null && deadlineKind != null && hasOwnEffort != null

private fun validateCanonicalItem(item: RemoteProposalCanonicalItem) {
    requireCanonicalUuid(item.id)
    require(item.title.isNotBlank() && item.title == item.title.trim())
    require(item.title.length <= 500)
    require(item.notes == null || item.notes.length <= 100_000)
    require(item.timezoneName.isNotBlank())
    requireNotNull(runCatching { ZoneId.of(item.timezoneName) }.getOrNull())
    require(item.durationSeconds == null || item.durationSeconds in 1..MAX_DURATION_SECONDS)
    item.blockedByItemId?.let(::requireCanonicalUuid)
    val deadline = item.deadlineAt?.let(::requireInstant)
    val earliest = item.earliestStartAt?.let(::requireInstant)
    require(deadline == null || earliest == null || earliest < deadline)
    item.recurrence?.let { recurrence ->
        require(recurrence is JsonObject)
        require(encodedJsonBytes(recurrence) <= MAX_RECURRENCE_BYTES)
    }
    require(encodedJsonBytes(item.flexibleConstraints) <= MAX_CONSTRAINT_BYTES)
    validateSplitPolicy(item.splitPolicy, item.durationSeconds)
    require(item.importance in 0..100 && item.urgency in 0..100)
    item.parentId?.let { parentId ->
        requireCanonicalUuid(parentId)
        require(parentId != item.id)
    }
    require(item.siblingOrder in 0..1_000_000)
    require(item.revision > 0)
    val created = requireInstant(item.createdAt)
    val updated = requireInstant(item.updatedAt)
    require(updated >= created)
    val completed = item.completedAt?.let(::requireInstant)
    val deleted = item.deletedAt?.let(::requireInstant)
    require((item.status == RemoteProposalItemStatus.COMPLETED) == (completed != null))
    require(completed == null || completed >= created)
    require(deleted == null || deleted >= created)
    require(deleted == null || !item.isExecutable)

    val structuralAnchors = listOf(item.durationKind, item.deadlineKind, item.hasOwnEffort)
    val hasAnyStructuralAnchor = structuralAnchors.any { it != null }
    val hasExplicitStructuralMetadata = structuralAnchors.all { it != null }
    val hasStructuralCompanion = listOf(
        item.durationMinSeconds,
        item.durationMaxSeconds,
        item.durationSource,
        item.deadlineDate,
        item.deadlineStrength,
        item.deadlineSoftWeight,
        item.blockedReasonKind,
        item.blockedByItemId,
        item.blockedReason,
    ).any { it != null }
    require(
        hasAnyStructuralAnchor == hasExplicitStructuralMetadata &&
            (hasAnyStructuralAnchor || !hasStructuralCompanion),
    )
    val legacy = CanonicalItemSnapshot(
        id = item.id,
        isSensitive = item.isSensitive,
        kind = item.kind.wireValue,
        status = item.status.wireValue,
        title = item.title,
        notes = item.notes,
        timezoneName = item.timezoneName,
        durationSeconds = item.durationSeconds,
        deadlineAt = item.deadlineAt,
        earliestStartAt = item.earliestStartAt,
        recurrenceJson = item.recurrence?.toString(),
        flexibleConstraintsJson = item.flexibleConstraints.toString(),
        splitPolicyJson = item.splitPolicy.toString(),
        importance = item.importance,
        urgency = item.urgency,
        parentId = item.parentId,
        siblingOrder = item.siblingOrder,
        isExecutable = item.isExecutable,
        revision = item.revision,
        createdAt = item.createdAt,
        updatedAt = item.updatedAt,
        completedAt = item.completedAt,
        deletedAt = item.deletedAt,
    )
    val structural = if (hasExplicitStructuralMetadata) {
        legacy.copy(
            durationKind = requireNotNull(item.durationKind),
            durationMinSeconds = item.durationMinSeconds,
            durationMaxSeconds = item.durationMaxSeconds,
            durationSource = item.durationSource,
            deadlineKind = requireNotNull(item.deadlineKind),
            deadlineDate = item.deadlineDate,
            deadlineStrength = item.deadlineStrength,
            deadlineSoftWeight = item.deadlineSoftWeight,
            hasOwnEffort = requireNotNull(item.hasOwnEffort),
            blockedReasonKind = item.blockedReasonKind,
            blockedByItemId = item.blockedByItemId,
            blockedReason = item.blockedReason,
            hasExplicitStructuralMetadata = true,
        )
    } else {
        legacy
    }
    structural.requireValidStructuralMetadata()
}

private fun validateSplitPolicy(policy: JsonObject, durationSeconds: Long?) {
    val type = policy["type"]?.jsonPrimitive?.contentOrNull
    when (type) {
        "indivisible" -> require(policy.keys == setOf("type"))
        "splittable" -> {
            require(
                policy.keys == setOf(
                    "type",
                    "minimum_chunk_seconds",
                    "maximum_chunk_seconds",
                ),
            )
            val minimum = policy["minimum_chunk_seconds"]?.jsonPrimitive?.longOrNull
            val maximum = policy["maximum_chunk_seconds"]?.jsonPrimitive?.longOrNull
            require(durationSeconds != null && minimum != null && maximum != null)
            require(minimum > 0 && maximum >= minimum && maximum <= durationSeconds)
        }
        else -> throw IllegalArgumentException("Unsupported proposal split policy")
    }
}

private fun validateReceipt(receipt: RemoteProposalApplicationReceipt) {
    requireCanonicalUuid(receipt.applicationId)
    require(receipt.proposals.size in 1..MAX_PROPOSALS_PER_PREVIEW)
    require(receipt.proposals.map { member -> member.proposalId }.toSet().size == receipt.proposals.size)
    receipt.proposals.forEach { member ->
        requireCanonicalUuid(member.proposalId)
        require(member.appliedRevision > 0)
    }
    require(receipt.commandIds.size in 1..MAX_PROPOSAL_COMMANDS)
    require(receipt.commandIds.toSet().size == receipt.commandIds.size)
    receipt.commandIds.forEach(::requireCanonicalUuid)
    require(receipt.affectedItemIds.isNotEmpty())
    require(receipt.affectedItemIds.toSet().size == receipt.affectedItemIds.size)
    receipt.affectedItemIds.forEach(::requireCanonicalUuid)
    val appliedAt = requireInstant(receipt.appliedAt)
    val undoExpiresAt = requireInstant(receipt.undoExpiresAt)
    require(undoExpiresAt > appliedAt)
    val undoneAt = receipt.undoneAt?.let(::requireInstant)
    when (receipt.status) {
        RemoteProposalApplicationStatus.APPLIED -> {
            require(receipt.applicationRevision == 1L)
            require(undoneAt == null)
        }
        RemoteProposalApplicationStatus.UNDONE -> {
            require(receipt.applicationRevision == 2L)
            require(undoneAt != null && undoneAt >= appliedAt && undoneAt <= undoExpiresAt)
        }
    }
}

private fun requireReviewHash(value: String) {
    require(REVIEW_HASH.matches(value))
}

private fun requireCanonicalUuid(value: String) {
    val parsed = runCatching { UUID.fromString(value) }.getOrNull()
    require(parsed != null && parsed.toString() == value)
}

private fun requireInstant(value: String): Instant =
    requireNotNull(runCatching { Instant.parse(value) }.getOrNull())

private fun requireBoundedSummary(value: String) {
    require(value.isNotBlank() && value.length <= 1_000)
}

private fun validateIdempotencyKey(value: String) {
    require(
        value.length in 8..128 && value.all { character ->
            character in '0'..'9' || character in 'A'..'Z' || character in 'a'..'z' ||
                character in setOf('-', '_', '.', '~')
        },
    )
}

private fun encodedJsonBytes(value: JsonElement): Int =
    proposalApplicationJson().encodeToString(JsonElement.serializer(), value)
        .toByteArray(StandardCharsets.UTF_8)
        .size

private inline fun <T> invalidRequest(block: () -> T): T = try {
    block()
} catch (error: ProposalApplicationApiException) {
    throw error
} catch (error: SerializationException) {
    throw ProposalApplicationApiException.InvalidRequest(error)
} catch (error: IllegalArgumentException) {
    throw ProposalApplicationApiException.InvalidRequest(error)
}

private inline fun validateResponse(block: () -> Unit) {
    try {
        block()
    } catch (error: IllegalArgumentException) {
        throw ProposalApplicationApiException.InvalidResponse(error)
    }
}

private const val PROPOSAL_APPLICATION_METHOD = "POST"
private const val PROPOSAL_APPLICATION_ACCEPT = "application/json"
private const val PROPOSAL_APPLICATION_CONTENT_TYPE = "application/json; charset=utf-8"
private const val PROPOSAL_APPLICATION_CACHE_CONTROL = "no-store"
private const val PROPOSAL_APPLICATION_PRAGMA = "no-cache"
private const val MAX_MUTATION_BODY_BYTES = 4 * 1024
private const val MAX_PROPOSALS_PER_PREVIEW = 20
private const val MAX_PROPOSAL_COMMANDS = 100
private const val MAX_DURATION_SECONDS = 366L * 24 * 60 * 60
private const val MAX_RECURRENCE_BYTES = 16 * 1024
private const val MAX_CONSTRAINT_BYTES = 32 * 1024
private val PROPOSAL_STRUCTURAL_WIRE_KEYS = setOf(
    "duration_kind",
    "duration_min_seconds",
    "duration_max_seconds",
    "duration_source",
    "deadline_kind",
    "deadline_date",
    "deadline_strength",
    "deadline_soft_weight",
    "has_own_effort",
    "blocked_reason_kind",
    "blocked_by_item_id",
    "blocked_reason",
)
private val MATERIAL_PROPOSAL_ITEM_FIELDS = listOf(
    RemoteProposalItemField.IS_SENSITIVE,
    RemoteProposalItemField.KIND,
    RemoteProposalItemField.STATUS,
    RemoteProposalItemField.TITLE,
    RemoteProposalItemField.NOTES,
    RemoteProposalItemField.TIMEZONE_NAME,
    RemoteProposalItemField.DURATION_KIND,
    RemoteProposalItemField.DURATION_SECONDS,
    RemoteProposalItemField.DURATION_MIN_SECONDS,
    RemoteProposalItemField.DURATION_MAX_SECONDS,
    RemoteProposalItemField.DURATION_SOURCE,
    RemoteProposalItemField.DEADLINE_KIND,
    RemoteProposalItemField.DEADLINE_DATE,
    RemoteProposalItemField.DEADLINE_AT,
    RemoteProposalItemField.DEADLINE_STRENGTH,
    RemoteProposalItemField.DEADLINE_SOFT_WEIGHT,
    RemoteProposalItemField.EARLIEST_START_AT,
    RemoteProposalItemField.RECURRENCE,
    RemoteProposalItemField.FLEXIBLE_CONSTRAINTS,
    RemoteProposalItemField.DEPENDENCIES,
    RemoteProposalItemField.HAS_OWN_EFFORT,
    RemoteProposalItemField.SPLIT_POLICY,
    RemoteProposalItemField.IMPORTANCE,
    RemoteProposalItemField.URGENCY,
    RemoteProposalItemField.PARENT_ID,
    RemoteProposalItemField.SIBLING_ORDER,
    RemoteProposalItemField.BLOCKED_REASON_KIND,
    RemoteProposalItemField.BLOCKED_BY_ITEM_ID,
    RemoteProposalItemField.BLOCKED_REASON,
    RemoteProposalItemField.IS_EXECUTABLE,
    RemoteProposalItemField.REVISION,
    RemoteProposalItemField.COMPLETED_AT,
    RemoteProposalItemField.DELETED_AT,
)
private val LEGACY_MATERIAL_PROPOSAL_ITEM_FIELDS = MATERIAL_PROPOSAL_ITEM_FIELDS.filterNot {
    it in setOf(
        RemoteProposalItemField.DURATION_KIND,
        RemoteProposalItemField.DURATION_MIN_SECONDS,
        RemoteProposalItemField.DURATION_MAX_SECONDS,
        RemoteProposalItemField.DURATION_SOURCE,
        RemoteProposalItemField.DEADLINE_KIND,
        RemoteProposalItemField.DEADLINE_DATE,
        RemoteProposalItemField.DEADLINE_STRENGTH,
        RemoteProposalItemField.DEADLINE_SOFT_WEIGHT,
        RemoteProposalItemField.HAS_OWN_EFFORT,
        RemoteProposalItemField.BLOCKED_REASON_KIND,
        RemoteProposalItemField.BLOCKED_BY_ITEM_ID,
        RemoteProposalItemField.BLOCKED_REASON,
        RemoteProposalItemField.DEPENDENCIES,
    )
}
private val REVIEW_HASH = Regex("^sha256:[0-9a-fA-F]{64}$")
