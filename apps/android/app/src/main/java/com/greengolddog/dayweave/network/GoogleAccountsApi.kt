package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
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
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
data class RemoteGoogleAccount(
    val id: String,
    @SerialName("external_account_id") val externalAccountId: String,
    @SerialName("display_label") val displayLabel: String,
    val status: String,
    @SerialName("sync_enabled") val syncEnabled: Boolean,
    @SerialName("is_default") val isDefault: Boolean,
    @SerialName("granted_scopes") val grantedScopes: Set<String>,
    @SerialName("token_expires_at") val tokenExpiresAt: String?,
    val revision: Long,
    @SerialName("created_at") val createdAt: String,
    @SerialName("updated_at") val updatedAt: String,
)

@Serializable
data class RemoteGoogleCleanupStatus(
    val held: Long,
    val pending: Long,
    val retrying: Long,
    val exhausted: Long,
    @SerialName("volatile_guardians") val volatileGuardians: Long,
    @SerialName("durability_degraded") val durabilityDegraded: Boolean,
    @SerialName("revocation_fenced") val revocationFenced: Boolean,
    @SerialName("operator_recovery_required") val operatorRecoveryRequired: Boolean,
    @SerialName("uncertain_authorizations") val uncertainAuthorizations: Long,
    @SerialName("legacy_recovery_required") val legacyRecoveryRequired: Long,
    @SerialName("next_attempt_at") val nextAttemptAt: String?,
    @SerialName("last_failure_at") val lastFailureAt: String?,
)

@Serializable
data class RemoteGoogleAccounts(
    val accounts: List<RemoteGoogleAccount>,
    val cleanup: RemoteGoogleCleanupStatus,
)

@Serializable
data class RemoteGoogleAuthorization(
    @SerialName("authorization_url") val authorizationUrl: String,
    @SerialName("expires_at") val expiresAt: String,
)

@Serializable
data class StartGoogleAuthorizationRequest(
    val services: Set<String>,
    @SerialName("force_consent") val forceConsent: Boolean,
    @SerialName("login_hint") val loginHint: String? = null,
    @SerialName("account_id") val accountId: String? = null,
    @SerialName("connect_new") val connectNew: Boolean,
    @SerialName("make_default") val makeDefault: Boolean,
)

@Serializable
private data class GoogleAccountRevisionRequest(
    @SerialName("expected_revision") val expectedRevision: Long,
)

sealed class GoogleAccountsApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : GoogleAccountsApiException("The DayWeave API rejected the bearer token")

    class Conflict : GoogleAccountsApiException("The Google connection changed on the server")

    class Validation(val statusCode: Int) : GoogleAccountsApiException(
        "The DayWeave API rejected the Google connection request with HTTP $statusCode",
    )

    class Unavailable : GoogleAccountsApiException("Google authorization is not ready on the server")

    class Http(val statusCode: Int) : GoogleAccountsApiException(
        "The DayWeave API returned HTTP $statusCode",
    )

    class InvalidResponse(cause: Throwable? = null) : GoogleAccountsApiException(
        "The DayWeave API returned an unreadable Google account response",
        cause,
    )
}

interface GoogleAccountsTransport {
    suspend fun accounts(configuration: AuthenticatedApiConfiguration): RemoteGoogleAccounts

    suspend fun startAuthorization(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: StartGoogleAuthorizationRequest,
    ): RemoteGoogleAuthorization

    suspend fun setPaused(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        paused: Boolean,
        idempotencyKey: String,
    ): RemoteGoogleAccount

    suspend fun disconnect(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        idempotencyKey: String,
    ): RemoteGoogleAccount
}

class OkHttpGoogleAccountsTransport(
    private val client: OkHttpClient = defaultClient(),
    private val json: Json = defaultJson(),
) : GoogleAccountsTransport {
    override suspend fun accounts(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteGoogleAccounts {
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/integrations/google/accounts")
            .build()
        return execute(requestBuilder(configuration, url.toString()).get().build(), 200)
    }

    override suspend fun startAuthorization(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: StartGoogleAuthorizationRequest,
    ): RemoteGoogleAuthorization {
        validateIdempotencyKey(idempotencyKey)
        require(request.services.isNotEmpty() && request.services.all { it in GOOGLE_SERVICES })
        require(!(request.connectNew && request.accountId != null))
        request.accountId?.let(::validateUuid)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/integrations/google/oauth/start")
            .build()
        val body = json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)
        return execute(
            requestBuilder(configuration, url.toString())
                .header("Idempotency-Key", idempotencyKey)
                .post(body)
                .build(),
            201,
        )
    }

    override suspend fun setPaused(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        paused: Boolean,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        validateUuid(accountId)
        validateRevision(expectedRevision)
        validateIdempotencyKey(idempotencyKey)
        val action = if (paused) "pause" else "resume"
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/integrations/google/accounts")
            .addPathSegment(accountId)
            .addPathSegment(action)
            .build()
        val body = json.encodeToString(GoogleAccountRevisionRequest(expectedRevision))
            .toRequestBody(JSON_MEDIA_TYPE)
        return execute(
            requestBuilder(configuration, url.toString())
                .header("Idempotency-Key", idempotencyKey)
                .post(body)
                .build(),
            200,
        )
    }

    override suspend fun disconnect(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        validateUuid(accountId)
        validateRevision(expectedRevision)
        validateIdempotencyKey(idempotencyKey)
        val url = configuration.baseUrl.newBuilder()
            .addPathSegments("v1/integrations/google/accounts")
            .addPathSegment(accountId)
            .addQueryParameter("expected_revision", expectedRevision.toString())
            .build()
        return execute(
            requestBuilder(configuration, url.toString())
                .header("Idempotency-Key", idempotencyKey)
                .delete()
                .build(),
            200,
        )
    }

    private fun requestBuilder(
        configuration: AuthenticatedApiConfiguration,
        url: String,
    ): Request.Builder = Request.Builder()
        .url(url)
        .tag(AuthenticatedApiConfiguration::class.java, configuration)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer ${configuration.bearerToken}")

    private suspend inline fun <reified T> execute(request: Request, expectedStatus: Int): T {
        val configuration = request.tag(AuthenticatedApiConfiguration::class.java)
            ?: throw GoogleAccountsApiException.InvalidResponse()
        val response = configuration.executeAuthenticated(client, request)
        response.use {
            if (response.code != expectedStatus) throw response.toGoogleAccountsApiException()
            val responseText = response.body.charStream().use { it.readBoundedGoogleText() }
            try {
                return json.decodeFromString<T>(responseText)
            } catch (error: SerializationException) {
                throw GoogleAccountsApiException.InvalidResponse(error)
            } catch (error: IllegalArgumentException) {
                throw GoogleAccountsApiException.InvalidResponse(error)
            }
        }
    }

    private fun Response.toGoogleAccountsApiException(): GoogleAccountsApiException = when (code) {
        401 -> GoogleAccountsApiException.Authentication()
        409 -> GoogleAccountsApiException.Conflict()
        400, 422 -> GoogleAccountsApiException.Validation(code)
        503 -> GoogleAccountsApiException.Unavailable()
        else -> GoogleAccountsApiException.Http(code)
    }

    companion object {
        private const val MAX_RESPONSE_CHARS = 2 * 1024 * 1024
        private val GOOGLE_SERVICES = setOf("calendar", "tasks")
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
            ignoreUnknownKeys = false
            // Required nullable response members must still be present on the wire.
            explicitNulls = true
            encodeDefaults = true
        }

        private fun validateRevision(revision: Long) {
            require(revision > 0) { "Google account revision must be positive" }
        }

        private fun validateUuid(value: String) {
            require(UUID.fromString(value).toString() == value) { "Google account ID is invalid" }
        }

        private fun validateIdempotencyKey(value: String) {
            require(
                value.length in 8..128 && value.all { character ->
                    character in '0'..'9' || character in 'A'..'Z' || character in 'a'..'z' ||
                        character in setOf('.', '_', '-')
                },
            ) { "Google idempotency key is invalid" }
        }

        private fun Reader.readBoundedGoogleText(): String {
            val result = StringBuilder()
            val buffer = CharArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = read(buffer)
                if (read < 0) break
                if (result.length + read > MAX_RESPONSE_CHARS) {
                    throw GoogleAccountsApiException.InvalidResponse()
                }
                result.append(buffer, 0, read)
            }
            return result.toString()
        }
    }
}

@OptIn(ExperimentalCoroutinesApi::class)
private suspend fun Call.awaitGoogleAccountsResponse(): Response =
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
