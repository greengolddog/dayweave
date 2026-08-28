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
import kotlinx.serialization.json.JsonObject
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
data class RemoteSuggestion(
    val id: String,
    val revision: Long,
    @SerialName("submitted_by") val submittedBy: String,
    val source: String,
    @SerialName("source_reference") val sourceReference: String? = null,
    val kind: String,
    val status: String,
    val title: String,
    val explanation: String? = null,
    val payload: JsonObject,
    @SerialName("decision_note") val decisionNote: String? = null,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
    @SerialName("expires_at") val expiresAt: String,
    @SerialName("decided_at") val decidedAt: String? = null,
)

@Serializable
private data class SuggestionListEnvelope(val suggestions: List<RemoteSuggestion>)

@Serializable
private data class SuggestionEnvelope(val suggestion: RemoteSuggestion)

@Serializable
private data class EditSuggestionRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
    val title: String,
    val explanation: String,
)

@Serializable
private data class DecisionRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
)

sealed class SuggestionApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : SuggestionApiException("The DayWeave API rejected the bearer token")

    class Conflict : SuggestionApiException("The suggestion changed on the server")

    class Http(val statusCode: Int) : SuggestionApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : SuggestionApiException(
        "The DayWeave API returned an unreadable response",
        cause,
    )
}

interface SuggestionsTransport {
    suspend fun list(configuration: AuthenticatedApiConfiguration): List<RemoteSuggestion>

    suspend fun edit(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
        title: String,
        explanation: String,
    ): RemoteSuggestion

    suspend fun accept(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion

    suspend fun reject(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion
}

class OkHttpSuggestionsTransport(
    private val client: OkHttpClient = defaultClient(),
    private val json: Json = defaultJson(),
) : SuggestionsTransport {
    override suspend fun list(
        configuration: AuthenticatedApiConfiguration,
    ): List<RemoteSuggestion> {
        val url = suggestionsUrl(configuration)
            .newBuilder()
            .addQueryParameter("limit", MAX_LIST_LIMIT.toString())
            .build()
        val request = requestBuilder(configuration, url.toString()).get().build()
        return execute<SuggestionListEnvelope>(request).suggestions
    }

    override suspend fun edit(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
        title: String,
        explanation: String,
    ): RemoteSuggestion {
        val body = json.encodeToString(
            EditSuggestionRequest(
                expectedRevision = expectedRevision,
                title = title,
                explanation = explanation,
            ),
        )
        val request = requestBuilder(configuration, suggestionUrl(configuration, id))
            .patch(body.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        return execute<SuggestionEnvelope>(request).suggestion
    }

    override suspend fun accept(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion = decide(configuration, id, expectedRevision, "accept")

    override suspend fun reject(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
    ): RemoteSuggestion = decide(configuration, id, expectedRevision, "reject")

    private suspend fun decide(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        expectedRevision: Long,
        decision: String,
    ): RemoteSuggestion {
        val body = json.encodeToString(DecisionRequest(expectedRevision))
        val request = requestBuilder(
            configuration,
            suggestionUrl(configuration, id, decision),
        )
            .post(body.toRequestBody(JSON_MEDIA_TYPE))
            .build()
        return execute<SuggestionEnvelope>(request).suggestion
    }

    private fun suggestionsUrl(configuration: AuthenticatedApiConfiguration) =
        configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions")
            .build()

    private fun suggestionUrl(
        configuration: AuthenticatedApiConfiguration,
        id: String,
        action: String? = null,
    ): String {
        val builder = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/suggestions")
            .addPathSegment(id)
        if (action != null) builder.addPathSegment(action)
        return builder.build().toString()
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(request: Request): T {
        val response = client.newCall(request).await()
        response.use {
            val responseText = response.body.charStream().use { reader -> reader.readBoundedText() }
            if (!response.isSuccessful) throw response.toApiException()
            try {
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw SuggestionApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw SuggestionApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toApiException(): SuggestionApiException = when (code) {
            401 -> SuggestionApiException.Authentication()
            409 -> SuggestionApiException.Conflict()
            else -> SuggestionApiException.Http(code)
        }

    companion object {
        private const val MAX_LIST_LIMIT = 200
        private const val MAX_RESPONSE_CHARS = 2 * 1024 * 1024
        private val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .writeTimeout(30, TimeUnit.SECONDS)
            .callTimeout(45, TimeUnit.SECONDS)
            .retryOnConnectionFailure(true)
            // Authentication is scoped to the configured origin. Treat every
            // redirect as an API error instead of risking credential replay to
            // a different host or an unexpected endpoint.
            .followRedirects(false)
            .followSslRedirects(false)
            .build()

        fun defaultJson(): Json = Json {
            ignoreUnknownKeys = true
            explicitNulls = false
            encodeDefaults = true
        }

        private fun Reader.readBoundedText(): String {
            val result = StringBuilder()
            val buffer = CharArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = read(buffer)
                if (read < 0) break
                if (result.length + read > MAX_RESPONSE_CHARS) {
                    throw SuggestionApiException.InvalidResponse()
                }
                result.append(buffer, 0, read)
            }
            return result.toString()
        }
    }
}

@OptIn(ExperimentalCoroutinesApi::class)
private suspend fun Call.await(): Response = suspendCancellableCoroutine { continuation ->
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
