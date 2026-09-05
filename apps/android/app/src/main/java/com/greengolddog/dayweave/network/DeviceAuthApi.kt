package com.greengolddog.dayweave.network

import java.io.IOException
import java.io.Reader
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.Base64
import java.util.UUID
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
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

@Serializable
internal class CreateDeviceEnrollmentRequest(
    val id: String,
    @SerialName("enrollment_token") val enrollmentToken: String,
    @SerialName("client_instance_id") val clientInstanceId: String,
    @SerialName("client_kind") val clientKind: String = "android",
    @SerialName("device_label") val deviceLabel: String,
    val scopes: List<String> = ANDROID_DEVICE_AUTH_SCOPES,
    @SerialName("client_contract_version") val clientContractVersion: Int =
        DEVICE_AUTH_CONTRACT_VERSION,
    @SerialName("client_version") val clientVersion: String,
    @SerialName("client_capabilities") val clientCapabilities: List<String> =
        ANDROID_DEVICE_AUTH_CAPABILITIES,
) {
    override fun toString(): String =
        "CreateDeviceEnrollmentRequest(id=$id, enrollmentToken=<redacted>, " +
            "clientInstanceId=$clientInstanceId, clientKind=$clientKind)"
}

@Serializable
internal class DeviceEnrollmentIssuedResponse(
    val id: String,
    @SerialName("enrollment_token") val enrollmentToken: String,
    @SerialName("expires_at") val expiresAt: String,
    @SerialName("client_contract_version") val clientContractVersion: Int,
    val replayed: Boolean,
) {
    override fun toString(): String =
        "DeviceEnrollmentIssuedResponse(id=$id, enrollmentToken=<redacted>)"
}

@Serializable
internal class ConsumeDeviceEnrollmentRequest(
    @SerialName("session_id") val sessionId: String,
    @SerialName("access_token") val accessToken: String,
    @SerialName("refresh_token") val refreshToken: String,
) {
    override fun toString(): String =
        "ConsumeDeviceEnrollmentRequest(sessionId=$sessionId, accessToken=<redacted>, refreshToken=<redacted>)"
}

@Serializable
internal class RefreshDeviceSessionRequest(
    @SerialName("next_access_token") val nextAccessToken: String,
    @SerialName("next_refresh_token") val nextRefreshToken: String,
) {
    override fun toString(): String =
        "RefreshDeviceSessionRequest(nextAccessToken=<redacted>, nextRefreshToken=<redacted>)"
}

@Serializable
internal data class DeviceSessionMutationResponse(
    val session: DeviceSessionContract,
    val replayed: Boolean,
)

@Serializable
private data class DeviceAuthErrorEnvelope(val error: DeviceAuthErrorBody)

@Serializable
private data class DeviceAuthErrorBody(
    val code: String,
    val message: String,
    val details: JsonElement? = null,
)

internal sealed class DeviceAuthApiException(message: String, cause: Throwable? = null) :
    IOException(message, cause) {
    class Authentication : DeviceAuthApiException("Device authentication was rejected")
    class Forbidden : DeviceAuthApiException("Device authentication scope was rejected")
    class Conflict : DeviceAuthApiException("Device authentication state conflicts")
    class Validation : DeviceAuthApiException("Device authentication input was rejected")
    class Unavailable : DeviceAuthApiException("Durable device authentication is unavailable")
    class Http(val statusCode: Int) :
        DeviceAuthApiException("Device authentication returned HTTP $statusCode")
    class InvalidResponse :
        DeviceAuthApiException("Device authentication returned an incompatible response")
}

internal fun DeviceAuthApiException.isDeterministicRejection(): Boolean = when (this) {
    is DeviceAuthApiException.Authentication,
    is DeviceAuthApiException.Forbidden,
    is DeviceAuthApiException.Conflict,
    is DeviceAuthApiException.Validation,
    is DeviceAuthApiException.Http,
    is DeviceAuthApiException.InvalidResponse,
    -> true
    is DeviceAuthApiException.Unavailable -> false
}

internal interface DeviceAuthTransport {
    suspend fun createEnrollment(
        request: DeviceEnrollmentCreationHttpRequest,
    ): DeviceEnrollmentIssuedResponse

    suspend fun consumeEnrollment(
        baseUrl: String,
        enrollmentToken: String,
        request: ConsumeDeviceEnrollmentRequest,
    ): DeviceSessionMutationResponse

    suspend fun refreshSession(
        baseUrl: String,
        refreshToken: String,
        request: RefreshDeviceSessionRequest,
    ): DeviceSessionMutationResponse

    /** Requires the current-session revocation contract: 204 with an empty body. */
    suspend fun revokeSession(baseUrl: String, accessToken: String, sessionId: String)
}

internal class OkHttpDeviceAuthTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val now: () -> Instant = Instant::now,
    private val allowCleartextLoopbackForTests: Boolean = false,
) : DeviceAuthTransport {
    private val json = DEVICE_AUTH_JSON

    override suspend fun createEnrollment(
        request: DeviceEnrollmentCreationHttpRequest,
    ): DeviceEnrollmentIssuedResponse {
        val decodedRequest = validateEnrollmentCreationHttpRequest(
            request,
            allowCleartextLoopbackForTests,
        )
        val bodyBytes = decodeCanonicalBody(request.bodyBase64Url.value)
        return try {
            val httpRequest = Request.Builder()
                .url(request.url)
                .method(request.method, bodyBytes.toRequestBody(JSON_MEDIA_TYPE))
                .header("Accept", request.acceptHeader)
                .header("Content-Type", request.contentTypeHeader)
                .header("Authorization", request.authorizationHeader.value)
                .header("Cache-Control", request.cacheControlHeader)
                .header("Pragma", request.pragmaHeader)
                .build()
            val response = execute(httpRequest, expectedStatuses = setOf(200, 201))
            val status = response.code
            val issued = decode<DeviceEnrollmentIssuedResponse>(response)
            validateEnrollmentResponse(issued, decodedRequest, status)
            issued
        } finally {
            bodyBytes.fill(0)
        }
    }

    override suspend fun consumeEnrollment(
        baseUrl: String,
        enrollmentToken: String,
        request: ConsumeDeviceEnrollmentRequest,
    ): DeviceSessionMutationResponse {
        validateExactDeviceToken(enrollmentToken, DEVICE_ENROLLMENT_TOKEN_PREFIX)
        requireUuid(request.sessionId)
        validateExactDeviceToken(request.accessToken, DEVICE_ACCESS_TOKEN_PREFIX)
        validateExactDeviceToken(request.refreshToken, DEVICE_REFRESH_TOKEN_PREFIX)
        val response = execute(
            request = requestBuilder(
                baseUrl,
                "v1/auth/device-enrollments/consume",
                enrollmentToken,
            )
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatuses = setOf(200, 201),
        )
        val status = response.code
        val mutation = decode<DeviceSessionMutationResponse>(response)
        if ((status == 200) != mutation.replayed || (status == 201) == mutation.replayed) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        return mutation
    }

    override suspend fun refreshSession(
        baseUrl: String,
        refreshToken: String,
        request: RefreshDeviceSessionRequest,
    ): DeviceSessionMutationResponse {
        validateExactDeviceToken(refreshToken, DEVICE_REFRESH_TOKEN_PREFIX)
        validateExactDeviceToken(request.nextAccessToken, DEVICE_ACCESS_TOKEN_PREFIX)
        validateExactDeviceToken(request.nextRefreshToken, DEVICE_REFRESH_TOKEN_PREFIX)
        val response = execute(
            request = requestBuilder(baseUrl, "v1/auth/sessions/refresh", refreshToken)
                .post(json.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE))
                .build(),
            expectedStatuses = setOf(200),
        )
        return decode(response)
    }

    override suspend fun revokeSession(
        baseUrl: String,
        accessToken: String,
        sessionId: String,
    ) {
        validateExactDeviceToken(accessToken, DEVICE_ACCESS_TOKEN_PREFIX)
        requireUuid(sessionId)
        val response = executeCurrentSessionRevoke(
            requestBuilder(baseUrl, "v1/auth/sessions/$sessionId", accessToken)
                .delete()
                .build(),
        )
        response.use {
            val body = try {
                if (response.body.contentLength() > MAX_RESPONSE_CHARS) {
                    throw DeviceAuthApiException.InvalidResponse()
                }
                response.body.charStream().use { reader ->
                    reader.readBoundedDeviceAuthText()
                }
            } catch (_: DeviceAuthApiException.InvalidResponse) {
                throw DeviceSessionDeleteOutcomeAmbiguousException()
            }
            if (body.isNotEmpty()) throw DeviceSessionDeleteOutcomeAmbiguousException()
        }
    }

    private fun requestBuilder(baseUrl: String, path: String, bearer: String): Request.Builder {
        val normalized = normalizeBaseUrlForDeviceAuth(baseUrl, allowCleartextLoopbackForTests)
        val url = normalized.newBuilder().addPathSegments(path).build()
        return Request.Builder()
            .url(url)
            .header("Accept", "application/json")
            .header("Authorization", "Bearer $bearer")
            .header("Cache-Control", "no-store")
            .header("Pragma", "no-cache")
    }

    private suspend fun execute(request: Request, expectedStatuses: Set<Int>): Response {
        val response = client.newCall(request).awaitDeviceAuthResponse()
        if (!hasStrictNoStoreHeaders(response)) {
            val exception = if (response.code in expectedStatuses) {
                DeviceAuthApiException.InvalidResponse()
            } else {
                DeviceAuthApiException.Unavailable()
            }
            response.close()
            throw exception
        }
        if (response.code !in expectedStatuses) {
            throw response.use(::decodeTrustedDeviceAuthError)
        }
        if (response.code != 204 && !hasExactJsonMediaType(response)) {
            response.close()
            throw DeviceAuthApiException.InvalidResponse()
        }
        return response
    }

    /**
     * Preserves whether a current-session DELETE is safe to retry/reconcile. Malformed success and
     * retryable upstream responses are outcome-ambiguous after dispatch. Malformed authentication
     * and deterministic client-error contracts are protocol failures and never prove retirement.
     */
    private suspend fun executeCurrentSessionRevoke(request: Request): Response {
        val response = client.newCall(request).awaitDeviceAuthResponse()
        when {
            response.code == 204 -> {
                if (!hasStrictNoStoreHeaders(response)) {
                    response.close()
                    throw DeviceSessionDeleteOutcomeAmbiguousException()
                }
                return response
            }
            response.code in RETRYABLE_MUTATION_STATUSES || response.code in 500..599 -> {
                response.close()
                throw DeviceAuthApiException.Unavailable()
            }
            response.code == 401 -> {
                val definitive = isTrustedDeviceAuthUnauthorized(response)
                response.close()
                throw if (definitive) {
                    DeviceAuthApiException.Authentication()
                } else {
                    DeviceAuthApiException.InvalidResponse()
                }
            }
            response.code in DETERMINISTIC_MUTATION_FAILURE_STATUSES -> {
                if (!hasStrictNoStoreHeaders(response) || !hasExactJsonMediaType(response)) {
                    response.close()
                    throw DeviceAuthApiException.InvalidResponse()
                }
                val failure = response.use(::decodeTrustedDeviceAuthError)
                throw if (failure is DeviceAuthApiException.Unavailable) {
                    DeviceAuthApiException.InvalidResponse()
                } else {
                    failure
                }
            }
            else -> {
                response.close()
                throw DeviceAuthApiException.InvalidResponse()
            }
        }
    }

    private inline fun <reified T> decode(response: Response): T = response.use {
        val body = response.body.charStream().use { reader ->
            reader.readBoundedDeviceAuthText()
        }
        try {
            json.decodeFromString<T>(body)
        } catch (_: SerializationException) {
            throw DeviceAuthApiException.InvalidResponse()
        } catch (_: IllegalArgumentException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
    }

    private fun validateEnrollmentResponse(
        response: DeviceEnrollmentIssuedResponse,
        request: CreateDeviceEnrollmentRequest,
        status: Int,
    ) {
        requireUuid(response.id)
        validateExactDeviceToken(response.enrollmentToken, DEVICE_ENROLLMENT_TOKEN_PREFIX)
        if (
            response.id != request.id ||
            response.enrollmentToken != request.enrollmentToken ||
            response.clientContractVersion != DEVICE_AUTH_CONTRACT_VERSION ||
            (status == 200) != response.replayed ||
            (status == 201) == response.replayed
        ) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        val expiry = try {
            Instant.parse(response.expiresAt)
        } catch (_: DateTimeParseException) {
            throw DeviceAuthApiException.InvalidResponse()
        }
        val current = now()
        if (
            !expiry.isAfter(current) ||
            expiry.isAfter(current.plus(ENROLLMENT_MAX_RESPONSE_WINDOW))
        ) {
            throw DeviceAuthApiException.InvalidResponse()
        }
    }

    private fun Reader.readBoundedDeviceAuthText(): String {
        val result = StringBuilder()
        val buffer = CharArray(DEFAULT_BUFFER_SIZE)
        while (true) {
            val read = read(buffer)
            if (read < 0) break
            if (result.length + read > MAX_RESPONSE_CHARS) {
                throw DeviceAuthApiException.InvalidResponse()
            }
            result.append(buffer, 0, read)
        }
        return result.toString()
    }

    private companion object {
        const val MAX_RESPONSE_CHARS = 64 * 1024
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
        val ENROLLMENT_MAX_RESPONSE_WINDOW: Duration = Duration.ofMinutes(15)
        val RETRYABLE_MUTATION_STATUSES = setOf(408, 425, 429)
        val DETERMINISTIC_MUTATION_FAILURE_STATUSES = setOf(400, 403, 404, 409, 422)
    }
}

internal fun buildEnrollmentCreationHttpRequest(
    baseUrl: String,
    bootstrapToken: String,
    request: CreateDeviceEnrollmentRequest,
    allowCleartextLoopback: Boolean,
): DeviceEnrollmentCreationHttpRequest {
    validateCreateEnrollmentBody(request)
    val bootstrap = validateLegacyBootstrapToken(bootstrapToken)
    val normalized = normalizeBaseUrlForDeviceAuth(baseUrl, allowCleartextLoopback)
    val url = normalized.newBuilder().addPathSegments(DEVICE_ENROLLMENT_CREATION_PATH).build()
    val bodyBytes = DEVICE_AUTH_JSON.encodeToString(request).toByteArray(StandardCharsets.UTF_8)
    return try {
        DeviceEnrollmentCreationHttpRequest(
            url = url.toString(),
            method = DEVICE_ENROLLMENT_CREATION_METHOD,
            acceptHeader = DEVICE_AUTH_ACCEPT_HEADER,
            contentTypeHeader = DEVICE_AUTH_CONTENT_TYPE_HEADER,
            cacheControlHeader = DEVICE_AUTH_REQUEST_CACHE_CONTROL,
            pragmaHeader = DEVICE_AUTH_REQUEST_PRAGMA,
            authorizationHeader = DeviceAuthSecret("Bearer $bootstrap"),
            bodyBase64Url = DeviceAuthSecret(
                Base64.getUrlEncoder().withoutPadding().encodeToString(bodyBytes),
            ),
        )
    } finally {
        bodyBytes.fill(0)
    }
}

internal fun validateEnrollmentCreationRequest(
    state: StoredDeviceAuthState.EnrollmentCreationPending,
) {
    val decoded = validateEnrollmentCreationHttpRequest(state.request, false)
    require(decoded.id == state.enrollmentId)
    require(decoded.enrollmentToken == state.enrollmentToken.value)
    require(decoded.clientInstanceId == state.clientInstanceId)
    require(decoded.deviceLabel == state.deviceLabel)
    require(decoded.clientVersion == state.clientVersion)
    require(decoded.scopes == state.scopes)
    require(decoded.clientCapabilities == state.capabilities)
    val bootstrap = state.request.authorizationHeader.value.removePrefix("Bearer ")
    require(
        buildEnrollmentCreationHttpRequest(
            state.baseUrl,
            bootstrap,
            decoded,
            allowCleartextLoopback = false,
        ) == state.request,
    )
}

internal fun validateEnrollmentCreationHttpRequest(
    request: DeviceEnrollmentCreationHttpRequest,
    allowCleartextLoopback: Boolean,
): CreateDeviceEnrollmentRequest {
    require(request.method == DEVICE_ENROLLMENT_CREATION_METHOD)
    require(request.acceptHeader == DEVICE_AUTH_ACCEPT_HEADER)
    require(request.contentTypeHeader == DEVICE_AUTH_CONTENT_TYPE_HEADER)
    require(request.cacheControlHeader == DEVICE_AUTH_REQUEST_CACHE_CONTROL)
    require(request.pragmaHeader == DEVICE_AUTH_REQUEST_PRAGMA)
    require(request.authorizationHeader.value.startsWith("Bearer "))
    validateLegacyBootstrapToken(request.authorizationHeader.value.removePrefix("Bearer "))
    val target = request.url.toHttpUrlOrNull()
        ?: throw IllegalArgumentException("Invalid enrollment creation URL")
    require(target.query == null && target.fragment == null)
    require(target.encodedPath.endsWith("/$DEVICE_ENROLLMENT_CREATION_PATH"))
    val basePath = target.encodedPath.removeSuffix(DEVICE_ENROLLMENT_CREATION_PATH)
    val base = target.newBuilder().encodedPath(basePath).build()
    require(
        normalizeBaseUrlForDeviceAuth(base.toString(), allowCleartextLoopback).toString() ==
            base.toString(),
    )
    val body = decodeCanonicalBody(request.bodyBase64Url.value)
    return try {
        val text = decodeStrictUtf8(body)
            ?: throw IllegalArgumentException("Invalid enrollment request encoding")
        val decoded = DEVICE_AUTH_JSON.decodeFromString<CreateDeviceEnrollmentRequest>(text)
        validateCreateEnrollmentBody(decoded)
        val canonical = DEVICE_AUTH_JSON.encodeToString(decoded).toByteArray(StandardCharsets.UTF_8)
        try {
            require(canonical.contentEquals(body))
        } finally {
            canonical.fill(0)
        }
        decoded
    } catch (_: SerializationException) {
        throw IllegalArgumentException("Invalid enrollment request body")
    } finally {
        body.fill(0)
    }
}

private fun validateCreateEnrollmentBody(request: CreateDeviceEnrollmentRequest) {
    requireUuid(request.id)
    validateExactDeviceToken(request.enrollmentToken, DEVICE_ENROLLMENT_TOKEN_PREFIX)
    requireUuid(request.clientInstanceId)
    require(request.clientKind == "android")
    requireValidDeviceIdentity(request.deviceLabel, request.clientVersion)
    require(request.scopes == ANDROID_DEVICE_AUTH_SCOPES)
    require(request.clientContractVersion == DEVICE_AUTH_CONTRACT_VERSION)
    require(request.clientCapabilities == ANDROID_DEVICE_AUTH_CAPABILITIES)
}

private fun decodeCanonicalBody(encoded: String): ByteArray {
    val decoded = try {
        Base64.getUrlDecoder().decode(encoded)
    } catch (_: IllegalArgumentException) {
        throw IllegalArgumentException("Invalid enrollment request body")
    }
    if (Base64.getUrlEncoder().withoutPadding().encodeToString(decoded) != encoded) {
        decoded.fill(0)
        throw IllegalArgumentException("Invalid enrollment request body")
    }
    return decoded
}

/** Only this exact server error contract may cause credential rotation or quarantine. */
internal fun isTrustedDeviceAuthUnauthorized(response: Response): Boolean {
    if (
        response.code != 401 ||
        !hasStrictNoStoreHeaders(response) ||
        !hasExactJsonMediaType(response) ||
        !hasExactHeader(response, "WWW-Authenticate", "Bearer realm=\"dayweave\"")
    ) {
        return false
    }
    val length = response.body.contentLength()
    if (length > MAX_DEVICE_AUTH_RESPONSE_BYTES) return false
    val peeked = response.peekBody(MAX_DEVICE_AUTH_RESPONSE_BYTES + 1L).bytes()
    return try {
        if (peeked.size > MAX_DEVICE_AUTH_RESPONSE_BYTES) return false
        val text = decodeStrictUtf8(peeked) ?: return false
        val outer = DEVICE_AUTH_JSON.parseToJsonElement(text) as? JsonObject ?: return false
        if (outer.keys != setOf("error")) return false
        val error = outer["error"] as? JsonObject ?: return false
        if (error.keys != setOf("code", "message")) return false
        val code = error["code"] as? JsonPrimitive ?: return false
        val message = error["message"] as? JsonPrimitive ?: return false
        code.isString && message.isString && code.content == "unauthorized" &&
            isSanitizedErrorText(message.content)
    } catch (_: SerializationException) {
        false
    } catch (_: IllegalArgumentException) {
        false
    } finally {
        peeked.fill(0)
    }
}

internal fun decodeTrustedDeviceAuthError(response: Response): DeviceAuthApiException {
    if (!hasExactJsonMediaType(response)) return DeviceAuthApiException.Unavailable()
    val status = response.code
    if (status == 401) {
        return if (isTrustedDeviceAuthUnauthorized(response)) {
            DeviceAuthApiException.Authentication()
        } else {
            DeviceAuthApiException.Unavailable()
        }
    }
    val expectedCode = when (status) {
        400 -> "invalid_json"
        403 -> "forbidden"
        404 -> "not_found"
        409 -> "conflict"
        422 -> "validation_failed"
        500 -> "internal_error"
        502 -> "bad_gateway"
        503 -> "service_unavailable"
        else -> return DeviceAuthApiException.Unavailable()
    }
    val bytes = try {
        response.body.byteStream().use(::readBoundedDeviceAuthErrorBytes)
    } catch (_: IOException) {
        return DeviceAuthApiException.Unavailable()
    }
    val envelope = try {
        val text = decodeStrictUtf8(bytes) ?: return DeviceAuthApiException.Unavailable()
        DEVICE_AUTH_JSON.decodeFromString<DeviceAuthErrorEnvelope>(text)
    } catch (_: SerializationException) {
        return DeviceAuthApiException.Unavailable()
    } catch (_: IllegalArgumentException) {
        return DeviceAuthApiException.Unavailable()
    } finally {
        bytes.fill(0)
    }
    if (
        envelope.error.code != expectedCode ||
        !isSanitizedErrorText(envelope.error.message)
    ) {
        return DeviceAuthApiException.Unavailable()
    }
    return when (status) {
        403 -> DeviceAuthApiException.Forbidden()
        409 -> DeviceAuthApiException.Conflict()
        400, 422 -> DeviceAuthApiException.Validation()
        500, 502, 503 -> DeviceAuthApiException.Unavailable()
        else -> DeviceAuthApiException.Http(status)
    }
}

private fun readBoundedDeviceAuthErrorBytes(input: java.io.InputStream): ByteArray {
    val buffer = ByteArray(MAX_DEVICE_AUTH_RESPONSE_BYTES + 1)
    var total = 0
    return try {
        while (total < buffer.size) {
            val read = input.read(buffer, total, buffer.size - total)
            if (read < 0) break
            total += read
        }
        if (total > MAX_DEVICE_AUTH_RESPONSE_BYTES || input.read() >= 0) {
            throw DeviceAuthApiException.Unavailable()
        }
        buffer.copyOf(total)
    } finally {
        buffer.fill(0)
    }
}

internal fun decodeStrictUtf8(bytes: ByteArray): String? = try {
    StandardCharsets.UTF_8.newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT)
        .onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(bytes))
        .toString()
} catch (_: java.nio.charset.CharacterCodingException) {
    null
}

internal fun hasStrictNoStoreHeaders(response: Response): Boolean {
    val directives = response.headers.values("Cache-Control")
        .flatMap { value -> value.split(',', limit = Int.MAX_VALUE) }
        .map { it.trim().lowercase() }
    return directives.size == 2 &&
        directives.toSet() == setOf("no-store", "max-age=0") &&
        response.headers.values("Pragma").let { values ->
            values.size == 1 && values.single().trim().lowercase() == "no-cache"
        }
}

internal fun hasExactJsonMediaType(response: Response): Boolean {
    val values = response.headers.values("Content-Type")
    if (values.size != 1) return false
    val components = values.single().split(';', limit = Int.MAX_VALUE)
    if (components.firstOrNull()?.trim()?.lowercase() != "application/json") return false
    return when (components.size) {
        1 -> true
        2 -> {
            val pair = components[1].trim().split('=', limit = 2)
            pair.size == 2 &&
                pair[0].trim().equals("charset", ignoreCase = true) &&
                pair[1].trim().equals("utf-8", ignoreCase = true)
        }
        else -> false
    }
}

private fun hasExactHeader(response: Response, name: String, value: String): Boolean =
    response.headers.values(name) == listOf(value)

private fun isSanitizedErrorText(value: String): Boolean =
    value.isNotBlank() && value.length <= MAX_DEVICE_AUTH_ERROR_TEXT_CHARS &&
        value.none { it.isISOControl() }

internal val DEVICE_AUTH_JSON = Json {
    ignoreUnknownKeys = false
    explicitNulls = true
    encodeDefaults = true
}

internal const val MAX_DEVICE_AUTH_RESPONSE_BYTES = 64 * 1024
private const val MAX_DEVICE_AUTH_ERROR_TEXT_CHARS = 512
private const val DEVICE_ENROLLMENT_CREATION_PATH = "v1/auth/device-enrollments"
private const val DEVICE_ENROLLMENT_CREATION_METHOD = "POST"
private const val DEVICE_AUTH_ACCEPT_HEADER = "application/json"
private const val DEVICE_AUTH_CONTENT_TYPE_HEADER = "application/json; charset=utf-8"
private const val DEVICE_AUTH_REQUEST_CACHE_CONTROL = "no-store"
private const val DEVICE_AUTH_REQUEST_PRAGMA = "no-cache"

private fun requireUuid(value: String) {
    if (!runCatching { UUID.fromString(value).toString() == value.lowercase() }.getOrDefault(false)) {
        throw DeviceAuthApiException.InvalidResponse()
    }
}

@OptIn(ExperimentalCoroutinesApi::class)
internal suspend fun Call.awaitDeviceAuthResponse(): Response =
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
