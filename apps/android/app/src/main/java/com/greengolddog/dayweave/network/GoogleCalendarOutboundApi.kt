package com.greengolddog.dayweave.network

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import okhttp3.MediaType
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okio.BufferedSink

/** Exact mutation requested by an explicitly reviewed Google outbound workflow. */
@Serializable
enum class GoogleCalendarOutboundOperation {
    @SerialName("upsert")
    UPSERT,

    @SerialName("delete")
    DELETE,
}

/** Provider entity whose exact projection is being reviewed. */
@Serializable
enum class GoogleCalendarOutboundEntityKind {
    @SerialName("calendar_event")
    CALENDAR_EVENT,

    @SerialName("task")
    TASK,
}

@Serializable
enum class ScheduleGooglePublicationOperation {
    @SerialName("create") CREATE,
    @SerialName("update") UPDATE,
    @SerialName("delete") DELETE,
    @SerialName("noop") NOOP,
}

@Serializable
enum class ScheduleGooglePublicationState {
    @SerialName("pending") PENDING,
    @SerialName("delivering") DELIVERING,
    @SerialName("backoff") BACKOFF,
    @SerialName("partially_published") PARTIALLY_PUBLISHED,
    @SerialName("published") PUBLISHED,
    @SerialName("conflict") CONFLICT,
    @SerialName("failed") FAILED,
    @SerialName("superseded") SUPERSEDED,
}

@Serializable
data class RemoteScheduleGooglePublicationChange(
    val ordinal: Int,
    @SerialName("slot_id") val slotId: String,
    @SerialName("source_block_id") val sourceBlockId: String?,
    val operation: ScheduleGooglePublicationOperation,
    @SerialName("provider_resource_id") val providerResourceId: String?,
    @SerialName("provider_etag") val providerEtag: String?,
    val summary: String,
    @SerialName("starts_at") val startsAt: String,
    @SerialName("ends_at") val endsAt: String,
) {
    override fun toString(): String =
        "RemoteScheduleGooglePublicationChange(ordinal=$ordinal, operation=$operation, " +
            "content=<redacted>)"
}

@Serializable
data class RemoteScheduleGooglePublicationPreview(
    val id: String,
    @SerialName("account_id") val accountId: String,
    @SerialName("collection_id") val collectionId: String,
    @SerialName("collection_revision") val collectionRevision: Long,
    @SerialName("collection_display_name") val collectionDisplayName: String,
    @SerialName("schedule_revision_id") val scheduleRevisionId: String,
    @SerialName("schedule_revision_number") val scheduleRevisionNumber: Long,
    @SerialName("preview_hash") val previewHash: String,
    @SerialName("create_count") val createCount: Int,
    @SerialName("update_count") val updateCount: Int,
    @SerialName("delete_count") val deleteCount: Int,
    @SerialName("noop_count") val noopCount: Int,
    val changes: List<RemoteScheduleGooglePublicationChange>,
    @SerialName("expires_at") val expiresAt: String,
) {
    override fun toString(): String =
        "RemoteScheduleGooglePublicationPreview(changeCount=${changes.size}, content=<redacted>)"
}

@Serializable
data class RemoteScheduleGooglePublicationApproval(
    @SerialName("preview_id") val previewId: String,
    @SerialName("approval_capability") val approvalCapability: String,
    @SerialName("expires_at") val expiresAt: String,
) {
    override fun toString(): String =
        "RemoteScheduleGooglePublicationApproval(capability=<redacted>)"
}

@Serializable
data class RemoteScheduleGooglePublicationAccepted(
    @SerialName("publication_id") val publicationId: String,
    val replayed: Boolean,
) {
    override fun toString(): String =
        "RemoteScheduleGooglePublicationAccepted(replayed=$replayed, id=<redacted>)"
}

@Serializable
data class RemoteScheduleGooglePublicationStatus(
    @SerialName("publication_id") val publicationId: String,
    @SerialName("account_id") val accountId: String,
    @SerialName("collection_id") val collectionId: String,
    @SerialName("schedule_revision_id") val scheduleRevisionId: String,
    val state: ScheduleGooglePublicationState,
    @SerialName("total_count") val totalCount: Int,
    @SerialName("pending_count") val pendingCount: Int,
    @SerialName("delivering_count") val deliveringCount: Int,
    @SerialName("published_count") val publishedCount: Int,
    @SerialName("conflicted_count") val conflictedCount: Int,
    @SerialName("failed_count") val failedCount: Int,
    @SerialName("superseded_count") val supersededCount: Int,
    @SerialName("created_at") val createdAt: String,
    @SerialName("completed_at") val completedAt: String?,
    @SerialName("last_error_code") val lastErrorCode: String?,
) {
    override fun toString(): String =
        "RemoteScheduleGooglePublicationStatus(state=$state, totalCount=$totalCount, " +
            "binding=<redacted>)"
}

@Serializable
data class RemoteGoogleOutboundPreview(
    val id: String,
    @SerialName("account_id") val accountId: String,
    @SerialName("collection_id") val collectionId: String,
    @SerialName("collection_revision") val collectionRevision: Long,
    @SerialName("collection_display_name") val collectionDisplayName: String,
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    @SerialName("entity_kind") val entityKind: GoogleCalendarOutboundEntityKind,
    val operation: GoogleCalendarOutboundOperation,
    @SerialName("provider_resource_id") val providerResourceId: String?,
    @SerialName("provider_etag") val providerEtag: String?,
    @SerialName("preview_hash") val previewHash: String,
    @SerialName("provider_payload") val providerPayload: JsonObject,
    @SerialName("expires_at") val expiresAt: String,
) {
    /** Provider payloads contain private event content and must never enter diagnostics. */
    override fun toString(): String =
        "RemoteGoogleOutboundPreview(entityKind=$entityKind, operation=$operation, " +
            "providerPayload=<redacted>)"
}

@Serializable
data class RemoteGoogleOutboundApproval(
    @SerialName("preview_id") val previewId: String,
    @SerialName("approval_capability") val approvalCapability: String,
    @SerialName("expires_at") val expiresAt: String,
) {
    /** The approval capability is a one-time bearer secret. */
    override fun toString(): String = "RemoteGoogleOutboundApproval(capability=<redacted>)"
}

@Serializable
data class RemoteGoogleOutboundAccepted(
    @SerialName("outbox_id") val outboxId: String,
    val replayed: Boolean,
) {
    override fun toString(): String = "RemoteGoogleOutboundAccepted(replayed=$replayed)"
}

@Serializable
private data class GoogleCalendarOutboundPreviewRequest(
    @SerialName("collection_id") val collectionId: String,
    @SerialName("item_id") val itemId: String,
    @SerialName("expected_item_revision") val expectedItemRevision: Long,
    val operation: GoogleCalendarOutboundOperation,
)

@Serializable
private data class GoogleCalendarOutboundApprovalRequest(
    @SerialName("expected_preview_hash") val expectedPreviewHash: String,
)

@Serializable
private data class GoogleCalendarOutboundEnqueueRequest(
    @SerialName("collection_id") val collectionId: String,
    @SerialName("item_id") val itemId: String,
    @SerialName("expected_item_revision") val expectedItemRevision: Long,
    val operation: GoogleCalendarOutboundOperation,
    @SerialName("approval_capability") val approvalCapability: String,
) {
    override fun toString(): String =
        "GoogleCalendarOutboundEnqueueRequest(operation=$operation, " +
            "approvalCapability=<redacted>)"
}

@Serializable
private data class RemoteGoogleOutboundPreviewEnvelope(
    val preview: RemoteGoogleOutboundPreview,
)

@Serializable
private data class RemoteGoogleOutboundApprovalEnvelope(
    val approval: RemoteGoogleOutboundApproval,
) {
    override fun toString(): String = "RemoteGoogleOutboundApprovalEnvelope(approval=<redacted>)"
}

@Serializable
private data class RemoteGoogleOutboundAcceptedEnvelope(
    val outbound: RemoteGoogleOutboundAccepted,
)

@Serializable
private data class ScheduleGooglePublicationPreviewRequest(
    @SerialName("collection_id") val collectionId: String,
    @SerialName("expected_schedule_revision_id") val expectedScheduleRevisionId: String,
)

@Serializable
private data class ScheduleGooglePublicationApprovalRequest(
    @SerialName("expected_preview_hash") val expectedPreviewHash: String,
)

@Serializable
private data class ScheduleGooglePublicationEnqueueRequest(
    @SerialName("preview_id") val previewId: String,
    @SerialName("collection_id") val collectionId: String,
    @SerialName("expected_schedule_revision_id") val expectedScheduleRevisionId: String,
    @SerialName("approval_capability") val approvalCapability: String,
) {
    override fun toString(): String =
        "ScheduleGooglePublicationEnqueueRequest(capability=<redacted>)"
}

sealed class GoogleCalendarOutboundApiException(message: String) : IOException(message) {
    class Authentication :
        GoogleCalendarOutboundApiException("The DayWeave API rejected the bearer token")

    class NotFound :
        GoogleCalendarOutboundApiException("The Google publication target was not found")

    class Conflict :
        GoogleCalendarOutboundApiException("The Google publication authority changed")

    class Validation(val statusCode: Int) : GoogleCalendarOutboundApiException(
        "The DayWeave API rejected the Google publication with HTTP $statusCode",
    )

    class Upstream :
        GoogleCalendarOutboundApiException("Google could not be reached by the server")

    class Unavailable :
        GoogleCalendarOutboundApiException("Google publication is unavailable")

    class Http(val statusCode: Int) : GoogleCalendarOutboundApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    /** Deliberately carries no decoder cause because it may contain private response material. */
    class InvalidResponse : GoogleCalendarOutboundApiException(
        "The DayWeave API returned an unreadable Google publication response",
    )
}

interface GoogleCalendarOutboundTransport {
    suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
        operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
    ): RemoteGoogleOutboundPreview

    suspend fun approve(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteGoogleOutboundApproval

    suspend fun enqueue(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
        approvalCapability: String,
        operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
    ): RemoteGoogleOutboundAccepted
}

interface GoogleSchedulePublicationTransport {
    suspend fun previewSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
    ): RemoteScheduleGooglePublicationPreview

    suspend fun approveSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteScheduleGooglePublicationApproval

    suspend fun enqueueSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
        approvalCapability: String,
    ): RemoteScheduleGooglePublicationAccepted

    suspend fun schedulePublicationStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        publicationId: String,
    ): RemoteScheduleGooglePublicationStatus
}

class OkHttpGoogleCalendarOutboundTransport(
    client: OkHttpClient = OkHttpGoogleAccountsTransport.defaultClient(),
    private val json: Json = defaultJson(),
) : GoogleCalendarOutboundTransport, GoogleSchedulePublicationTransport {
    // Durable outbound recovery, not OkHttp, owns every retry decision. In particular, approval is
    // one-shot and must become RESPONSE_UNKNOWN after an ambiguous dispatch.
    private val client = client.newBuilder()
        .retryOnConnectionFailure(false)
        .build()

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
        operation: GoogleCalendarOutboundOperation,
    ): RemoteGoogleOutboundPreview {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(collectionId, "Google collection ID")
        requireOutboundIdentity(itemId, "canonical item ID")
        requireOutboundRevision(expectedItemRevision)
        val request = GoogleCalendarOutboundPreviewRequest(
            collectionId = collectionId,
            itemId = itemId,
            expectedItemRevision = expectedItemRevision,
            operation = operation,
        )
        val url = accountUrl(configuration, accountId)
            .addPathSegments("outbound/previews")
            .build()
        val preview = execute<RemoteGoogleOutboundPreviewEnvelope>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 200,
        ).preview
        validatePreview(preview)
        if (
            preview.accountId != accountId ||
            preview.collectionId != collectionId ||
            preview.itemId != itemId ||
            preview.itemRevision != expectedItemRevision ||
            preview.operation != operation
        ) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
        return preview
    }

    override suspend fun approve(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteGoogleOutboundApproval {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(previewId, "Google outbound preview ID")
        require(validOutboundHash(expectedPreviewHash)) {
            "Google outbound preview hash is invalid"
        }
        val request = GoogleCalendarOutboundApprovalRequest(expectedPreviewHash)
        val url = accountUrl(configuration, accountId)
            .addPathSegments("outbound/previews")
            .addPathSegment(previewId)
            .addPathSegment("approve")
            .build()
        val approval = execute<RemoteGoogleOutboundApprovalEnvelope>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toOneShotOutboundRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 200,
        ).approval
        validateApproval(approval)
        if (approval.previewId != previewId) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
        return approval
    }

    override suspend fun enqueue(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
        approvalCapability: String,
        operation: GoogleCalendarOutboundOperation,
    ): RemoteGoogleOutboundAccepted {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(collectionId, "Google collection ID")
        requireOutboundIdentity(itemId, "canonical item ID")
        requireOutboundRevision(expectedItemRevision)
        require(validOutboundCapability(approvalCapability)) {
            "Google outbound approval capability is invalid"
        }
        val request = GoogleCalendarOutboundEnqueueRequest(
            collectionId = collectionId,
            itemId = itemId,
            expectedItemRevision = expectedItemRevision,
            operation = operation,
            approvalCapability = approvalCapability,
        )
        val url = accountUrl(configuration, accountId).addPathSegment("outbound").build()
        val accepted = execute<RemoteGoogleOutboundAcceptedEnvelope>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 202,
        ).outbound
        validateAccepted(accepted)
        return accepted
    }

    override suspend fun previewSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
    ): RemoteScheduleGooglePublicationPreview {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(collectionId, "Google collection ID")
        requireOutboundIdentity(expectedScheduleRevisionId, "published schedule revision ID")
        val request = ScheduleGooglePublicationPreviewRequest(
            collectionId = collectionId,
            expectedScheduleRevisionId = expectedScheduleRevisionId,
        )
        val url = accountUrl(configuration, accountId)
            .addPathSegments("schedule-publications/previews")
            .build()
        val preview = execute<RemoteScheduleGooglePublicationPreview>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 200,
        )
        validateSchedulePublicationPreview(preview)
        if (
            preview.accountId != accountId || preview.collectionId != collectionId ||
            preview.scheduleRevisionId != expectedScheduleRevisionId
        ) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
        return preview
    }

    override suspend fun approveSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteScheduleGooglePublicationApproval {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(previewId, "schedule publication preview ID")
        require(validOutboundHash(expectedPreviewHash))
        val request = ScheduleGooglePublicationApprovalRequest(expectedPreviewHash)
        val url = accountUrl(configuration, accountId)
            .addPathSegments("schedule-publications/previews")
            .addPathSegment(previewId)
            .addPathSegment("approve")
            .build()
        val approval = execute<RemoteScheduleGooglePublicationApproval>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toOneShotOutboundRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 200,
        )
        validateSchedulePublicationApproval(approval)
        if (approval.previewId != previewId) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
        return approval
    }

    override suspend fun enqueueSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
        approvalCapability: String,
    ): RemoteScheduleGooglePublicationAccepted {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(previewId, "schedule publication preview ID")
        requireOutboundIdentity(collectionId, "Google collection ID")
        requireOutboundIdentity(expectedScheduleRevisionId, "published schedule revision ID")
        require(validSchedulePublicationCapability(approvalCapability))
        val request = ScheduleGooglePublicationEnqueueRequest(
            previewId = previewId,
            collectionId = collectionId,
            expectedScheduleRevisionId = expectedScheduleRevisionId,
            approvalCapability = approvalCapability,
        )
        val url = accountUrl(configuration, accountId)
            .addPathSegment("schedule-publications")
            .build()
        val accepted = execute<RemoteScheduleGooglePublicationAccepted>(
            requestBuilder(configuration, url.toString())
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatus = 202,
        )
        validateSchedulePublicationAccepted(accepted)
        return accepted
    }

    override suspend fun schedulePublicationStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        publicationId: String,
    ): RemoteScheduleGooglePublicationStatus {
        requireOutboundIdentity(accountId, "Google account ID")
        requireOutboundIdentity(publicationId, "schedule publication ID")
        val url = accountUrl(configuration, accountId)
            .addPathSegment("schedule-publications")
            .addPathSegment(publicationId)
            .build()
        val status = execute<RemoteScheduleGooglePublicationStatus>(
            requestBuilder(configuration, url.toString()).get().build(),
            expectedStatus = 200,
        )
        validateSchedulePublicationStatus(status)
        if (status.accountId != accountId || status.publicationId != publicationId) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
        return status
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
            ?: throw GoogleCalendarOutboundApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != expectedStatus) throw response.toOutboundApiException()
            if (!response.hasStrictOutboundJsonMediaType() || !response.hasStrictOutboundNoStore()) {
                throw GoogleCalendarOutboundApiException.InvalidResponse()
            }
            val declaredLength = response.body.contentLength()
            if (declaredLength > MAX_RESPONSE_BYTES) {
                throw GoogleCalendarOutboundApiException.InvalidResponse()
            }
            val responseText = response.body.byteStream().use { it.readBoundedOutboundText() }
            try {
                if (StrictGoogleOutboundJsonObjectKeyScanner(responseText, json).hasDuplicateKeys()) {
                    throw GoogleCalendarOutboundApiException.InvalidResponse()
                }
                return json.decodeFromString<T>(responseText)
            } catch (error: GoogleCalendarOutboundApiException.InvalidResponse) {
                throw error
            } catch (_: SerializationException) {
                throw GoogleCalendarOutboundApiException.InvalidResponse()
            } catch (_: IllegalArgumentException) {
                throw GoogleCalendarOutboundApiException.InvalidResponse()
            }
        }
    }

    private fun Response.toOutboundApiException(): GoogleCalendarOutboundApiException = when (code) {
        401 -> GoogleCalendarOutboundApiException.Authentication()
        404 -> GoogleCalendarOutboundApiException.NotFound()
        409 -> GoogleCalendarOutboundApiException.Conflict()
        400, 422 -> GoogleCalendarOutboundApiException.Validation(code)
        502 -> GoogleCalendarOutboundApiException.Upstream()
        503 -> GoogleCalendarOutboundApiException.Unavailable()
        else -> GoogleCalendarOutboundApiException.Http(code)
    }

    private fun Response.hasStrictOutboundJsonMediaType(): Boolean {
        val value = headers.values("Content-Type").singleOrNull() ?: return false
        val mediaType = value.toMediaTypeOrNull() ?: return false
        if (mediaType.type != "application" || mediaType.subtype != "json") return false
        val components = value.split(';').map { it.trim().lowercase() }
        return components.firstOrNull() == "application/json" &&
            (components.size == 1 ||
                components.size == 2 && components[1].replace(" ", "") == "charset=utf-8")
    }

    private fun Response.hasStrictOutboundNoStore(): Boolean =
        headers.values("Cache-Control").singleOrNull()?.trim()?.lowercase() == "no-store"

    private fun java.io.InputStream.readBoundedOutboundText(): String {
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
        var total = 0L
        while (true) {
            val read = read(buffer)
            if (read < 0) break
            total += read
            if (total > MAX_RESPONSE_BYTES) {
                throw GoogleCalendarOutboundApiException.InvalidResponse()
            }
            output.write(buffer, 0, read)
        }
        return try {
            StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(output.toByteArray()))
                .toString()
        } catch (_: java.nio.charset.CharacterCodingException) {
            throw GoogleCalendarOutboundApiException.InvalidResponse()
        }
    }

    companion object {
        private const val MAX_RESPONSE_BYTES = 16L * 1024L * 1024L
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()

        fun defaultJson(): Json = Json {
            ignoreUnknownKeys = false
            // Nullable response members are required even when their value is null.
            explicitNulls = true
            encodeDefaults = true
        }
    }
}

private fun String.toOneShotOutboundRequestBody(contentType: MediaType): RequestBody {
    val bytes = toByteArray(StandardCharsets.UTF_8)
    return object : RequestBody() {
        override fun contentType() = contentType

        override fun contentLength(): Long = bytes.size.toLong()

        override fun isOneShot(): Boolean = true

        override fun writeTo(sink: BufferedSink) {
            sink.write(bytes)
        }
    }
}

private fun validatePreview(preview: RemoteGoogleOutboundPreview) {
    val providerBindingIsValid = when {
        preview.operation == GoogleCalendarOutboundOperation.UPSERT &&
            preview.providerResourceId == null && preview.providerEtag == null -> true
        preview.providerResourceId != null && preview.providerEtag != null ->
            validOutboundText(preview.providerResourceId, 2_048) &&
                validOutboundText(preview.providerEtag, 2_048)

        else -> false
    }
    val valid = canonicalOutboundUuidOrNull(preview.id)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.accountId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.collectionId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.itemId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        preview.collectionRevision > 0 &&
        preview.itemRevision > 0 &&
        validOutboundText(preview.collectionDisplayName, 4_096) &&
        providerBindingIsValid &&
        validOutboundHash(preview.previewHash) &&
        validProviderPayload(
            preview.providerPayload,
            maximumValueStringBytes = if (
                preview.entityKind == GoogleCalendarOutboundEntityKind.TASK
            ) {
                MAX_TASK_VALUE_STRING_BYTES
            } else {
                MAX_PROVIDER_VALUE_STRING_BYTES
            },
        ) &&
        validOutboundProviderProjection(
            preview.providerPayload,
            preview.entityKind,
            preview.operation,
        ) &&
        outboundInstantOrNull(preview.expiresAt) != null
    if (!valid) throw GoogleCalendarOutboundApiException.InvalidResponse()
}

private fun validateApproval(approval: RemoteGoogleOutboundApproval) {
    val valid = canonicalOutboundUuidOrNull(approval.previewId)?.let {
        it != ZERO_OUTBOUND_UUID
    } == true && validOutboundCapability(approval.approvalCapability) &&
        outboundInstantOrNull(approval.expiresAt) != null
    if (!valid) throw GoogleCalendarOutboundApiException.InvalidResponse()
}

private fun validateAccepted(accepted: RemoteGoogleOutboundAccepted) {
    if (canonicalOutboundUuidOrNull(accepted.outboxId)?.let { it != ZERO_OUTBOUND_UUID } != true) {
        throw GoogleCalendarOutboundApiException.InvalidResponse()
    }
}

private fun validateSchedulePublicationPreview(preview: RemoteScheduleGooglePublicationPreview) {
    val counts = listOf(
        preview.createCount,
        preview.updateCount,
        preview.deleteCount,
        preview.noopCount,
    )
    val valid = canonicalOutboundUuidOrNull(preview.id)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.accountId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.collectionId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        canonicalOutboundUuidOrNull(preview.scheduleRevisionId)?.let {
            it != ZERO_OUTBOUND_UUID
        } == true &&
        preview.collectionRevision > 0 && preview.scheduleRevisionNumber > 0 &&
        validOutboundText(preview.collectionDisplayName, MAX_SCHEDULE_COLLECTION_NAME_BYTES) &&
        validOutboundHash(preview.previewHash) &&
        counts.all { it >= 0 } && counts.sum() == preview.changes.size &&
        preview.changes.size <= MAX_SCHEDULE_PUBLICATION_CHANGES &&
        preview.createCount == preview.changes.count {
            it.operation == ScheduleGooglePublicationOperation.CREATE
        } &&
        preview.updateCount == preview.changes.count {
            it.operation == ScheduleGooglePublicationOperation.UPDATE
        } &&
        preview.deleteCount == preview.changes.count {
            it.operation == ScheduleGooglePublicationOperation.DELETE
        } &&
        preview.noopCount == preview.changes.count {
            it.operation == ScheduleGooglePublicationOperation.NOOP
        } &&
        preview.changes.withIndex().all { (index, change) ->
            validSchedulePublicationChange(change, index)
        } && preview.changes.map { it.slotId }.distinct().size == preview.changes.size &&
        outboundInstantOrNull(preview.expiresAt) != null
    if (!valid) throw GoogleCalendarOutboundApiException.InvalidResponse()
}

private fun validSchedulePublicationChange(
    change: RemoteScheduleGooglePublicationChange,
    expectedOrdinal: Int,
): Boolean {
    val providerBindingValid = when (change.operation) {
        ScheduleGooglePublicationOperation.CREATE ->
            change.providerResourceId == null && change.providerEtag == null
        ScheduleGooglePublicationOperation.UPDATE,
        ScheduleGooglePublicationOperation.DELETE,
        ScheduleGooglePublicationOperation.NOOP,
        -> change.providerResourceId?.let { validOutboundText(it, MAX_PROVIDER_BINDING_BYTES) } == true &&
            change.providerEtag?.let { validOutboundText(it, MAX_PROVIDER_BINDING_BYTES) } == true
    }
    val sourceBindingValid = when (change.operation) {
        ScheduleGooglePublicationOperation.CREATE,
        ScheduleGooglePublicationOperation.UPDATE,
        ScheduleGooglePublicationOperation.NOOP,
        -> change.sourceBlockId != null
        ScheduleGooglePublicationOperation.DELETE -> change.sourceBlockId == null
    }
    val start = outboundInstantOrNull(change.startsAt)
    val end = outboundInstantOrNull(change.endsAt)
    return change.ordinal == expectedOrdinal &&
        canonicalOutboundUuidOrNull(change.slotId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
        (change.sourceBlockId == null || canonicalOutboundUuidOrNull(change.sourceBlockId)
            ?.let { it != ZERO_OUTBOUND_UUID } == true) &&
        sourceBindingValid && providerBindingValid &&
        validOutboundText(change.summary, MAX_CALENDAR_SUMMARY_BYTES) &&
        change.summary.codePointCount(0, change.summary.length) <=
        MAX_CALENDAR_SUMMARY_CODE_POINTS &&
        start != null && end != null && start < end
}

private fun validateSchedulePublicationApproval(
    approval: RemoteScheduleGooglePublicationApproval,
) {
    if (
        canonicalOutboundUuidOrNull(approval.previewId)?.let { it != ZERO_OUTBOUND_UUID } != true ||
        !validSchedulePublicationCapability(approval.approvalCapability) ||
        outboundInstantOrNull(approval.expiresAt) == null
    ) {
        throw GoogleCalendarOutboundApiException.InvalidResponse()
    }
}

private fun validateSchedulePublicationAccepted(
    accepted: RemoteScheduleGooglePublicationAccepted,
) {
    if (
        canonicalOutboundUuidOrNull(accepted.publicationId)?.let {
            it != ZERO_OUTBOUND_UUID
        } != true
    ) {
        throw GoogleCalendarOutboundApiException.InvalidResponse()
    }
}

private fun validateSchedulePublicationStatus(status: RemoteScheduleGooglePublicationStatus) {
    val counts = listOf(
        status.pendingCount,
        status.deliveringCount,
        status.publishedCount,
        status.conflictedCount,
        status.failedCount,
        status.supersededCount,
    )
    val terminal = status.state in setOf(
        ScheduleGooglePublicationState.PARTIALLY_PUBLISHED,
        ScheduleGooglePublicationState.PUBLISHED,
        ScheduleGooglePublicationState.CONFLICT,
        ScheduleGooglePublicationState.FAILED,
        ScheduleGooglePublicationState.SUPERSEDED,
    )
    val stateMatchesCounts = when (status.state) {
        ScheduleGooglePublicationState.DELIVERING -> status.deliveringCount > 0
        ScheduleGooglePublicationState.PENDING,
        ScheduleGooglePublicationState.BACKOFF,
        -> status.deliveringCount == 0 && status.pendingCount > 0
        ScheduleGooglePublicationState.PUBLISHED -> status.publishedCount == status.totalCount
        ScheduleGooglePublicationState.PARTIALLY_PUBLISHED ->
            status.pendingCount == 0 && status.deliveringCount == 0 &&
                status.publishedCount in 1 until status.totalCount
        ScheduleGooglePublicationState.CONFLICT ->
            status.pendingCount == 0 && status.deliveringCount == 0 &&
                status.publishedCount == 0 && status.conflictedCount > 0
        ScheduleGooglePublicationState.FAILED ->
            status.pendingCount == 0 && status.deliveringCount == 0 &&
                status.publishedCount == 0 && status.conflictedCount == 0 &&
                status.failedCount > 0
        ScheduleGooglePublicationState.SUPERSEDED ->
            status.totalCount > 0 && status.supersededCount == status.totalCount &&
                status.pendingCount + status.deliveringCount + status.publishedCount +
                status.conflictedCount + status.failedCount == 0
    }
    val createdAt = outboundInstantOrNull(status.createdAt)
    val completedAt = status.completedAt?.let(::outboundInstantOrNull)
    val valid =
        canonicalOutboundUuidOrNull(status.publicationId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
            canonicalOutboundUuidOrNull(status.accountId)?.let { it != ZERO_OUTBOUND_UUID } == true &&
            canonicalOutboundUuidOrNull(status.collectionId)?.let {
                it != ZERO_OUTBOUND_UUID
            } == true &&
            canonicalOutboundUuidOrNull(status.scheduleRevisionId)?.let {
                it != ZERO_OUTBOUND_UUID
            } == true &&
            status.totalCount in 0..MAX_SCHEDULE_PUBLICATION_CHANGES &&
            counts.all { it in 0..MAX_SCHEDULE_PUBLICATION_CHANGES } &&
            counts.sum() == status.totalCount && stateMatchesCounts && createdAt != null &&
            (status.completedAt != null) == terminal &&
            (status.completedAt == null || completedAt != null && completedAt >= createdAt) &&
            (status.lastErrorCode == null || validSchedulePublicationErrorCode(status.lastErrorCode))
    if (!valid) throw GoogleCalendarOutboundApiException.InvalidResponse()
}

private fun requireOutboundIdentity(value: String, description: String) {
    require(canonicalOutboundUuidOrNull(value)?.let { it != ZERO_OUTBOUND_UUID } == true) {
        "$description is invalid"
    }
}

private fun requireOutboundRevision(value: Long) {
    require(value > 0) { "Canonical item revision must be positive" }
}

private fun canonicalOutboundUuidOrNull(value: String): UUID? = try {
    UUID.fromString(value).takeIf { it.toString() == value }
} catch (_: IllegalArgumentException) {
    null
}

private fun outboundInstantOrNull(value: String): Instant? = try {
    Instant.parse(value)
} catch (_: DateTimeParseException) {
    null
}

private fun validOutboundText(value: String, maximumUtf8Bytes: Int): Boolean =
    value.isNotEmpty() &&
        StandardCharsets.UTF_8.newEncoder().canEncode(value) &&
        value.toByteArray(StandardCharsets.UTF_8).size <= maximumUtf8Bytes &&
        value.none(Char::isISOControl)

private fun validOutboundHash(value: String): Boolean =
    value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }

private fun validOutboundCapability(value: String): Boolean {
    val prefix = "dw_ga1_"
    if (!value.startsWith(prefix)) return false
    val payload = value.substring(prefix.length)
    if (
        payload.length != 43 ||
        payload.any { character ->
            character !in 'A'..'Z' && character !in 'a'..'z' &&
                character !in '0'..'9' && character != '-' && character != '_'
        }
    ) {
        return false
    }
    return try {
        val decoded = Base64.getUrlDecoder().decode(payload)
        decoded.size == 32 && Base64.getUrlEncoder().withoutPadding()
            .encodeToString(decoded) == payload
    } catch (_: IllegalArgumentException) {
        false
    }
}

private fun validSchedulePublicationCapability(value: String): Boolean {
    val prefix = "dw_gsa1_"
    if (!value.startsWith(prefix)) return false
    val payload = value.substring(prefix.length)
    if (
        payload.length != 43 || payload.any { character ->
            character !in 'A'..'Z' && character !in 'a'..'z' &&
                character !in '0'..'9' && character != '-' && character != '_'
        }
    ) {
        return false
    }
    return try {
        val decoded = Base64.getUrlDecoder().decode(payload)
        decoded.size == 32 && Base64.getUrlEncoder().withoutPadding()
            .encodeToString(decoded) == payload
    } catch (_: IllegalArgumentException) {
        false
    }
}

private fun validSchedulePublicationErrorCode(value: String): Boolean =
    value.length in 1..MAX_SCHEDULE_ERROR_CODE_CHARS &&
        value.all { it in 'a'..'z' || it in '0'..'9' || it == '_' }

private fun validProviderPayload(
    payload: JsonObject,
    maximumValueStringBytes: Int = MAX_PROVIDER_VALUE_STRING_BYTES,
): Boolean {
    var nodes = 0
    var stringBytes = 0L

    fun consumeString(value: String, maximumBytes: Int, forbidControls: Boolean): Boolean {
        if (!StandardCharsets.UTF_8.newEncoder().canEncode(value)) return false
        val bytes = value.toByteArray(StandardCharsets.UTF_8).size
        if (bytes > maximumBytes || stringBytes + bytes > MAX_PROVIDER_STRING_BYTES) return false
        if (forbidControls && value.any(Char::isISOControl)) return false
        stringBytes += bytes
        return true
    }

    fun validate(element: JsonElement, depth: Int): Boolean {
        if (depth > MAX_PROVIDER_DEPTH || nodes >= MAX_PROVIDER_NODES) return false
        nodes += 1
        return when (element) {
            is JsonObject -> {
                element.size <= MAX_PROVIDER_CONTAINER_ENTRIES && element.all { (key, value) ->
                    consumeString(key, MAX_PROVIDER_KEY_BYTES, forbidControls = true) &&
                        validate(value, depth + 1)
                }
            }

            is JsonArray -> element.size <= MAX_PROVIDER_CONTAINER_ENTRIES &&
                element.all { validate(it, depth + 1) }

            is JsonPrimitive -> if (element.isString) {
                consumeString(
                    element.content,
                    maximumValueStringBytes,
                    forbidControls = false,
                )
            } else {
                element.content.length <= MAX_PROVIDER_NUMBER_BYTES
            }

            JsonNull -> true
        }
    }

    return payload.size <= MAX_PROVIDER_CONTAINER_ENTRIES && validate(payload, depth = 0)
}

private fun validOutboundProviderProjection(
    payload: JsonObject,
    entityKind: GoogleCalendarOutboundEntityKind,
    operation: GoogleCalendarOutboundOperation,
): Boolean = when (operation) {
    GoogleCalendarOutboundOperation.DELETE -> payload.isEmpty()
    GoogleCalendarOutboundOperation.UPSERT -> when (entityKind) {
        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT ->
            payload.isNotEmpty() && validPrivateFixedCalendarEvent(payload)
        GoogleCalendarOutboundEntityKind.TASK ->
            payload.isNotEmpty() && validGoogleTaskPreviewPayload(payload)
    }
}

/**
 * Exact review-only projection of the server's Google Task. Provider-managed hierarchy, links,
 * update metadata, deletion state, and hidden state must remain inert.
 */
private fun validGoogleTaskPreviewPayload(payload: JsonObject): Boolean {
    if (payload.keys != GOOGLE_TASK_ROOT_KEYS) return false
    val id = payload["id"].outboundStringOrNull()
    val title = payload["title"].outboundStringOrNull()
    val status = payload["status"].outboundStringOrNull()
    val completed = payload["completed"]
    return id != null && id.isEmpty() &&
        payload["etag"] == JsonNull &&
        title != null && validGoogleTaskTitle(title) &&
        validGoogleTaskNotes(payload["notes"]) &&
        status in setOf("needsAction", "completed") &&
        validGoogleTaskTimestamp(payload["due"], required = false) &&
        validGoogleTaskTimestamp(completed, required = status == "completed") &&
        (status == "completed") == (completed != JsonNull) &&
        payload["updated"] == JsonNull &&
        payload["parent"] == JsonNull &&
        payload["position"] == JsonNull &&
        payload["links"] == JsonNull &&
        payload["deleted"].outboundBooleanOrNull() == false &&
        payload["hidden"].outboundBooleanOrNull() == false
}

private fun validGoogleTaskTitle(value: String): Boolean =
    value.isNotEmpty() &&
        value.codePointCount(0, value.length) <= MAX_GOOGLE_TASK_TITLE_CODE_POINTS &&
        value == value.trim()

private fun validGoogleTaskNotes(value: JsonElement?): Boolean = when (value) {
    JsonNull -> true
    is JsonPrimitive -> value.isString && value.content.let { notes ->
        notes.isNotEmpty() &&
            notes.codePointCount(0, notes.length) <= MAX_GOOGLE_TASK_NOTES_CODE_POINTS &&
            notes == notes.trim() &&
            notes.all { character -> character == '\n' || !character.isISOControl() } &&
            notes.split('\n').all { line -> line.isNotEmpty() && line == line.trim() } &&
            !containsLegacyGoogleTaskMarker(notes)
    }
    else -> false
}

private fun containsLegacyGoogleTaskMarker(value: String): Boolean {
    val lower = value.lowercase()
    var searchFrom = 0
    while (searchFrom < lower.length) {
        val marker = lower.indexOf("[dayweave", startIndex = searchFrom)
        if (marker < 0) return false
        var suffix = marker + "[dayweave".length
        while (suffix < lower.length && lower[suffix].isWhitespace()) suffix += 1
        if (lower.startsWith("item:", startIndex = suffix)) return true
        searchFrom = marker + "[dayweave".length
    }
    return false
}

private fun validGoogleTaskTimestamp(value: JsonElement?, required: Boolean): Boolean = when (value) {
    JsonNull -> !required
    is JsonPrimitive -> value.isString && value.content.let { timestamp ->
        timestamp.isNotEmpty() && timestamp.toByteArray(StandardCharsets.UTF_8).size <= 64 &&
            offsetDateTimeOrNull(timestamp) != null
    }
    else -> false
}

/**
 * Defense in depth for the first Android publication slice. The server's canonical event encoder
 * includes these sensitive fields even when empty, so absence or any non-empty value fails closed.
 */
private fun validPrivateFixedCalendarEvent(payload: JsonObject): Boolean {
    if (payload.keys != CALENDAR_EVENT_ROOT_KEYS) return false
    val providerId = payload["id"].outboundStringOrNull()
    val summary = payload["summary"] as? JsonPrimitive
    val status = payload["status"] as? JsonPrimitive
    val transparency = payload["transparency"] as? JsonPrimitive
    val visibility = payload["visibility"] as? JsonPrimitive
    val eventType = payload["eventType"] as? JsonPrimitive
    val start = payload["start"] as? JsonObject
    val end = payload["end"] as? JsonObject
    val boundariesAreValid = start != null && end != null && validCalendarBoundaries(start, end)
    val requiredShapeIsValid = providerId.validDayWeaveCalendarEventId() &&
        payload["etag"] == JsonNull &&
        summary?.isString == true &&
        validOutboundText(summary.content, MAX_CALENDAR_SUMMARY_BYTES) &&
        summary.content.codePointCount(0, summary.content.length) <= MAX_CALENDAR_SUMMARY_CODE_POINTS &&
        payload["description"].validCalendarDescription() &&
        payload["location"] == JsonNull &&
        status?.isString == true && status.content == "confirmed" &&
        transparency?.isString == true && transparency.content == "opaque" &&
        visibility?.isString == true && visibility.content == "private" &&
        eventType?.isString == true && eventType.content == "default" && boundariesAreValid
    val collaborationFieldsAreInert =
        (payload["attendees"] as? JsonArray)?.isEmpty() == true &&
            (payload["attachments"] as? JsonArray)?.isEmpty() == true &&
            (payload["recurrence"] as? JsonArray)?.isEmpty() == true &&
            payload["conferenceData"] == JsonNull &&
            payload["recurringEventId"] == JsonNull &&
            payload["originalStartTime"] == JsonNull &&
            payload["updated"] == JsonNull &&
            payload["sequence"] == JsonNull &&
            payload["extendedProperties"].validDayWeaveOwnershipProof()
    return requiredShapeIsValid && collaborationFieldsAreInert
}

private fun String?.validDayWeaveCalendarEventId(): Boolean =
    this != null && length in MIN_CALENDAR_EVENT_ID_CHARS..MAX_CALENDAR_EVENT_ID_CHARS &&
        first() == 'd' && drop(1).all { it in '0'..'9' || it in 'a'..'f' }

private fun JsonElement?.validCalendarDescription(): Boolean = when (this) {
    JsonNull -> true
    is JsonPrimitive -> isString &&
        StandardCharsets.UTF_8.newEncoder().canEncode(content) &&
        content.toByteArray(StandardCharsets.UTF_8).size <= MAX_CALENDAR_DESCRIPTION_BYTES &&
        content.codePointCount(0, content.length) <= MAX_CALENDAR_DESCRIPTION_CODE_POINTS &&
        content.all { it == '\n' || it == '\t' || !it.isISOControl() }
    else -> false
}

private fun JsonElement?.validDayWeaveOwnershipProof(): Boolean {
    val properties = this as? JsonObject ?: return false
    if (properties.keys != CALENDAR_EXTENDED_PROPERTY_KEYS) return false
    val privateProperties = properties["private"] as? JsonObject ?: return false
    val sharedProperties = properties["shared"] as? JsonObject ?: return false
    if (privateProperties.keys != CALENDAR_PRIVATE_PROPERTY_KEYS || sharedProperties.isNotEmpty()) {
        return false
    }
    return privateProperties[DAYWEAVE_OWNERSHIP_PROOF_KEY].outboundStringOrNull() ==
        DAYWEAVE_REDACTED_OWNERSHIP_PROOF
}

private fun validCalendarBoundaries(start: JsonObject, end: JsonObject): Boolean {
    if (start.keys != CALENDAR_BOUNDARY_KEYS || end.keys != CALENDAR_BOUNDARY_KEYS) return false
    val startDate = start["date"].outboundStringOrNull()
    val endDate = end["date"].outboundStringOrNull()
    val startTime = start["dateTime"].outboundStringOrNull()
    val endTime = end["dateTime"].outboundStringOrNull()
    val startZone = start["timeZone"].outboundStringOrNull()
    val endZone = end["timeZone"].outboundStringOrNull()
    if (startZone == null || endZone == null || startZone != endZone || !validZoneId(startZone)) {
        return false
    }
    return when {
        startDate == null && endDate == null && startTime != null && endTime != null -> {
            val parsedStart = offsetDateTimeOrNull(startTime)
            val parsedEnd = offsetDateTimeOrNull(endTime)
            parsedStart != null && parsedEnd != null && parsedStart.isBefore(parsedEnd)
        }

        else -> false
    }
}

private fun JsonElement?.outboundStringOrNull(): String? = when (this) {
    JsonNull, null -> null
    is JsonPrimitive -> content.takeIf { isString }
    else -> null
}

private fun JsonElement?.outboundBooleanOrNull(): Boolean? =
    (this as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull

private fun offsetDateTimeOrNull(value: String): OffsetDateTime? = try {
    OffsetDateTime.parse(value, DateTimeFormatter.ISO_OFFSET_DATE_TIME)
} catch (_: DateTimeParseException) {
    null
}

private fun validZoneId(value: String): Boolean = try {
    ZoneId.of(value)
    true
} catch (_: java.time.DateTimeException) {
    false
}

/** Detects duplicate object keys, including equivalent escaped spellings, before decoding. */
private class StrictGoogleOutboundJsonObjectKeyScanner(
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
                            require(source.getOrNull(index)?.isOutboundHexDigit() == true)
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

    private fun Char.isOutboundHexDigit(): Boolean =
        this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

    private companion object {
        const val MAX_JSON_DEPTH = 64
        val JSON_WHITESPACE = setOf(' ', '\t', '\r', '\n')
        val PRIMITIVE_DELIMITERS = JSON_WHITESPACE + setOf(',', ']', '}')
    }
}

private const val MAX_PROVIDER_DEPTH = 32
private const val MAX_PROVIDER_NODES = 20_000
private const val MAX_PROVIDER_STRING_BYTES = 1024L * 1024L
private const val MAX_PROVIDER_CONTAINER_ENTRIES = 10_000
private const val MAX_PROVIDER_KEY_BYTES = 1_024
private const val MAX_PROVIDER_VALUE_STRING_BYTES = 256 * 1_024
private const val MAX_TASK_VALUE_STRING_BYTES = 400_000
private const val MAX_PROVIDER_NUMBER_BYTES = 128
private const val MAX_CALENDAR_SUMMARY_BYTES = 8 * 1024
private const val MAX_CALENDAR_SUMMARY_CODE_POINTS = 500
private const val MAX_CALENDAR_DESCRIPTION_BYTES = 256 * 1024
private const val MAX_CALENDAR_DESCRIPTION_CODE_POINTS = 100_000
private const val MAX_GOOGLE_TASK_TITLE_CODE_POINTS = 500
private const val MAX_GOOGLE_TASK_NOTES_CODE_POINTS = 100_000
private const val MAX_SCHEDULE_PUBLICATION_CHANGES = 10_000
private const val MAX_SCHEDULE_COLLECTION_NAME_BYTES = 4_096
private const val MAX_PROVIDER_BINDING_BYTES = 2_048
private const val MAX_SCHEDULE_ERROR_CODE_CHARS = 128
private const val MIN_CALENDAR_EVENT_ID_CHARS = 66
private const val MAX_CALENDAR_EVENT_ID_CHARS = 73
private const val DAYWEAVE_OWNERSHIP_PROOF_KEY = "dayweaveOwnershipProof"
private const val DAYWEAVE_REDACTED_OWNERSHIP_PROOF = "[server-managed]"
private val CALENDAR_BOUNDARY_KEYS = setOf("date", "dateTime", "timeZone")
private val CALENDAR_EVENT_ROOT_KEYS = setOf(
    "id",
    "etag",
    "status",
    "summary",
    "description",
    "location",
    "start",
    "end",
    "recurringEventId",
    "originalStartTime",
    "recurrence",
    "transparency",
    "visibility",
    "eventType",
    "attendees",
    "conferenceData",
    "attachments",
    "updated",
    "sequence",
    "extendedProperties",
)
private val CALENDAR_EXTENDED_PROPERTY_KEYS = setOf("private", "shared")
private val CALENDAR_PRIVATE_PROPERTY_KEYS = setOf(DAYWEAVE_OWNERSHIP_PROOF_KEY)
private val GOOGLE_TASK_ROOT_KEYS = setOf(
    "id",
    "etag",
    "title",
    "notes",
    "status",
    "due",
    "completed",
    "updated",
    "parent",
    "position",
    "links",
    "deleted",
    "hidden",
)
private val ZERO_OUTBOUND_UUID = UUID(0, 0)
