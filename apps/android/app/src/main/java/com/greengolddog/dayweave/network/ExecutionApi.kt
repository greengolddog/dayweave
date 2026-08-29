package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
data class RemoteExecutionSession(
    val id: String,
    @SerialName("item_id") val itemId: String,
    @SerialName("item_revision") val itemRevision: Long,
    @SerialName("occurrence_id") val occurrenceId: String?,
    @SerialName("session_index") val sessionIndex: Int,
    @SerialName("planned_block_id") val plannedBlockId: String?,
    @SerialName("source_device_id") val sourceDeviceId: String,
    val status: String,
    val revision: Long,
    @SerialName("accumulated_seconds") val accumulatedSeconds: Long,
    @SerialName("actual_seconds") val actualSeconds: Long?,
    @SerialName("started_at") val startedAt: String,
    @SerialName("running_since") val runningSince: String?,
    @SerialName("paused_at") val pausedAt: String?,
    @SerialName("pause_until") val pauseUntil: String?,
    @SerialName("pause_reason") val pauseReason: String?,
    @SerialName("ended_at") val endedAt: String?,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
data class RemoteExecutionSnapshot(
    val revision: Long,
    @SerialName("active_session") val activeSession: RemoteExecutionSession?,
)

@Serializable
data class RemoteExecutionMutation(
    val revision: Long,
    @SerialName("active_session") val activeSession: RemoteExecutionSession?,
    @SerialName("changed_session") val changedSession: RemoteExecutionSession,
    val replayed: Boolean,
)

data class RemoteExecutionHistoryPage(
    val sessions: List<RemoteExecutionSession>,
    val nextOffset: Long?,
)

@Serializable
private data class ExecutionSnapshotEnvelope(val execution: RemoteExecutionSnapshot)

@Serializable
private data class ExecutionMutationEnvelope(val mutation: RemoteExecutionMutation)

@Serializable
private data class ExecutionHistoryEnvelope(
    val sessions: List<RemoteExecutionSession>,
    @SerialName("next_offset") val nextOffset: Long?,
)

sealed class ExecutionApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : ExecutionApiException("The DayWeave API rejected the bearer token")

    class NotFound : ExecutionApiException("The execution resource was not found")

    class Conflict : ExecutionApiException("The execution lease changed on another device")

    class Validation(val statusCode: Int) : ExecutionApiException(
        "The DayWeave API rejected an execution command with HTTP $statusCode",
    )

    class Http(val statusCode: Int) : ExecutionApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : ExecutionApiException(
        "The DayWeave API returned an unreadable execution response",
        cause,
    )
}

interface ExecutionTransport {
    suspend fun snapshot(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteExecutionSnapshot

    /** [requestJson] is the exact durable body and must never be re-encoded on a retry. */
    suspend fun command(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteExecutionMutation

    suspend fun history(
        configuration: AuthenticatedApiConfiguration,
        limit: Int = 100,
        offset: Long = 0,
    ): RemoteExecutionHistoryPage
}

class OkHttpExecutionTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val json: Json = Json {
        ignoreUnknownKeys = false
        // Every nullable response member is still required by the Rust wire contract.
        explicitNulls = true
        encodeDefaults = true
    },
) : ExecutionTransport {
    override suspend fun snapshot(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteExecutionSnapshot {
        val url = configuration.baseUrl.newBuilder().addPathSegments("v1/execution").build()
        return execute<ExecutionSnapshotEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
        ).execution
    }

    override suspend fun command(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        requestJson: String,
    ): RemoteExecutionMutation {
        if (
            idempotencyKey.length !in 8..128 ||
            idempotencyKey.any { character ->
                !character.isAsciiLetterOrDigit() && character !in setOf('.', '_', ':', '-')
            }
        ) {
            throw ExecutionApiException.InvalidResponse()
        }
        if (requestJson.length !in 2..MAX_REQUEST_CHARS) {
            throw ExecutionApiException.InvalidResponse()
        }
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/execution/commands")
            .build()
        val request = requestBuilder(configuration, url.toString())
            .header("Idempotency-Key", idempotencyKey)
            .post(requestJson.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        return execute<ExecutionMutationEnvelope>(request).mutation
    }

    override suspend fun history(
        configuration: AuthenticatedApiConfiguration,
        limit: Int,
        offset: Long,
    ): RemoteExecutionHistoryPage {
        require(limit in 1..100)
        require(offset >= 0)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/execution/history")
            .addQueryParameter("limit", limit.toString())
            .addQueryParameter("offset", offset.toString())
            .build()
        val envelope = execute<ExecutionHistoryEnvelope>(
            requestBuilder(configuration, url.toString()).get().build(),
        )
        return RemoteExecutionHistoryPage(envelope.sessions, envelope.nextOffset)
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(request: Request): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw ExecutionApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != 200) throw response.toExecutionApiException()
            val responseText = response.body.charStream().use { reader ->
                reader.readBoundedExecutionText()
            }
            try {
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw ExecutionApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw ExecutionApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toExecutionApiException(): ExecutionApiException = when (code) {
        401 -> ExecutionApiException.Authentication()
        404 -> ExecutionApiException.NotFound()
        409 -> ExecutionApiException.Conflict()
        400, 422 -> ExecutionApiException.Validation(code)
        else -> ExecutionApiException.Http(code)
    }

    private fun Reader.readBoundedExecutionText(): String {
        val result = StringBuilder()
        val buffer = CharArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val read = read(buffer)
            if (read < 0) break
            if (result.length + read > MAX_RESPONSE_CHARS) {
                throw ExecutionApiException.InvalidResponse()
            }
            result.append(buffer, 0, read)
        }
        return result.toString()
    }

    private companion object {
        const val MAX_REQUEST_CHARS = 64 * 1024
        const val MAX_RESPONSE_CHARS = 256 * 1024
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
    }
}

private fun Char.isAsciiLetterOrDigit(): Boolean =
    this in 'a'..'z' || this in 'A'..'Z' || this in '0'..'9'

@OptIn(ExperimentalCoroutinesApi::class)
private suspend fun Call.awaitExecutionResponse(): Response =
    suspendCancellableCoroutine { continuation ->
        continuation.invokeOnCancellation { cancel() }
        enqueue(
            object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (continuation.isActive) continuation.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    if (continuation.isActive) {
                        continuation.resume(response) { _, value, _ -> value.close() }
                    } else {
                        response.close()
                    }
                }
            },
        )
    }
