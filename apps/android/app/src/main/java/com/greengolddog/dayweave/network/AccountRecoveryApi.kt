package com.greengolddog.dayweave.network

import java.io.IOException
import java.time.DateTimeException
import java.time.Duration
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.UUID
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.decodeFromJsonElement
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody

@Serializable
internal data class AccountRecoveryCodeContract(
    val id: String,
    @SerialName("created_at") val createdAt: String,
    val revision: Long,
)

@Serializable
internal data class CurrentAccountRecoveryCodeResponse(
    @SerialName("recovery_code") val recoveryCode: AccountRecoveryCodeContract?,
)

@Serializable
internal data class CreateAccountRecoveryCodeRequest(
    val id: String,
    @SerialName("recovery_code") val recoveryCode: String,
    @SerialName("replaces_recovery_code_id") val replacesRecoveryCodeId: String?,
    @SerialName("replaces_recovery_code_revision") val replacesRecoveryCodeRevision: Long?,
) {
    override fun toString(): String =
        "CreateAccountRecoveryCodeRequest(id=$id, recoveryCode=<redacted>)"
}

@Serializable
internal data class AccountRecoveryCodeMutationResponse(
    @SerialName("recovery_code") val recoveryCode: AccountRecoveryCodeContract,
    val replayed: Boolean,
)

@Serializable
internal data class ConsumeAccountRecoveryCodeRequest(
    @SerialName("session_id") val sessionId: String,
    @SerialName("access_token") val accessToken: String,
    @SerialName("refresh_token") val refreshToken: String,
    @SerialName("client_instance_id") val clientInstanceId: String,
    @SerialName("client_kind") val clientKind: String = "android",
    @SerialName("device_label") val deviceLabel: String,
    @SerialName("client_contract_version") val clientContractVersion: Int =
        DEVICE_AUTH_CONTRACT_VERSION,
    @SerialName("client_version") val clientVersion: String,
    @SerialName("client_capabilities") val clientCapabilities: List<String> =
        ANDROID_DEVICE_AUTH_CAPABILITIES,
    @SerialName("successor_recovery_code_id") val successorRecoveryCodeId: String,
    @SerialName("successor_recovery_code") val successorRecoveryCode: String,
) {
    override fun toString(): String =
        "ConsumeAccountRecoveryCodeRequest(sessionId=$sessionId, accessToken=<redacted>, " +
            "refreshToken=<redacted>, successorRecoveryCode=<redacted>)"
}

@Serializable
internal data class AccountRecoveryConsumptionResponse(
    val session: DeviceSessionContract,
    @SerialName("successor_recovery_code")
    val successorRecoveryCode: AccountRecoveryCodeContract,
    val replayed: Boolean,
)

internal interface AccountRecoveryTransport {
    suspend fun current(
        configuration: AuthenticatedApiConfiguration,
    ): CurrentAccountRecoveryCodeResponse

    suspend fun issue(
        configuration: AuthenticatedApiConfiguration,
        request: CreateAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryCodeMutationResponse

    suspend fun consume(
        baseUrl: String,
        recoveryCode: String,
        request: ConsumeAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryConsumptionResponse
}

/** Strict, no-store transport for client-journaled account recovery mutations. */
internal class OkHttpAccountRecoveryTransport(
    private val client: OkHttpClient = OkHttpCanonicalPlannerTransport.defaultClient(),
    private val now: () -> Instant = Instant::now,
    private val allowCleartextLoopbackForTests: Boolean = false,
) : AccountRecoveryTransport {
    override suspend fun current(
        configuration: AuthenticatedApiConfiguration,
    ): CurrentAccountRecoveryCodeResponse {
        val response = execute(
            configuration,
            requestBuilder(
                configuration.baseUrl.newBuilder()
                    .addPathSegments("v1/auth/recovery-codes/current")
                    .build()
                    .toString(),
                configuration.bearerToken,
            ).get().build(),
            setOf(200),
        )
        val (root, receivedAt) = decodeRoot(response)
        if (root.keys != CURRENT_RESPONSE_KEYS) invalid()
        val nested = root["recovery_code"]
        if (nested != null && nested !is kotlinx.serialization.json.JsonNull) {
            if ((nested as? JsonObject)?.keys != CODE_KEYS) invalid()
        }
        val decoded = decode<CurrentAccountRecoveryCodeResponse>(root)
        decoded.recoveryCode?.let { validateCode(it, receivedAt) }
        return decoded
    }

    override suspend fun issue(
        configuration: AuthenticatedApiConfiguration,
        request: CreateAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryCodeMutationResponse {
        validateIssueRequest(request)
        val response = execute(
            configuration,
            requestBuilder(
                configuration.baseUrl.newBuilder()
                    .addPathSegments("v1/auth/recovery-codes")
                    .build()
                    .toString(),
                configuration.bearerToken,
            ).post(DEVICE_AUTH_JSON.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)).build(),
            setOf(200, 201),
        )
        val status = response.code
        val (root, receivedAt) = decodeRoot(response)
        if (root.keys != MUTATION_RESPONSE_KEYS) invalid()
        if ((root["recovery_code"] as? JsonObject)?.keys != CODE_KEYS) invalid()
        val decoded = decode<AccountRecoveryCodeMutationResponse>(root)
        validateReplay(status, decoded.replayed)
        validateCode(decoded.recoveryCode, receivedAt)
        if (decoded.recoveryCode.id != request.id) invalid()
        validateMutationTime(decoded.recoveryCode.createdAt, preparedAt, receivedAt, decoded.replayed)
        return decoded
    }

    override suspend fun consume(
        baseUrl: String,
        recoveryCode: String,
        request: ConsumeAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryConsumptionResponse {
        validateExactDeviceToken(recoveryCode, ACCOUNT_RECOVERY_TOKEN_PREFIX)
        validateConsumeRequest(recoveryCode, request)
        val normalized = normalizeBaseUrlForDeviceAuth(baseUrl, allowCleartextLoopbackForTests)
        val response = execute(
            configuration = null,
            request = requestBuilder(
                normalized.newBuilder()
                    .addPathSegments("v1/auth/recovery-codes/consume")
                    .build()
                    .toString(),
                recoveryCode,
            ).post(DEVICE_AUTH_JSON.encodeToString(request).toRequestBody(JSON_MEDIA_TYPE)).build(),
            expectedStatuses = setOf(200, 201),
        )
        val status = response.code
        val (root, receivedAt) = decodeRoot(response)
        if (root.keys != CONSUMPTION_RESPONSE_KEYS) invalid()
        if ((root["session"] as? JsonObject)?.keys != SESSION_KEYS) invalid()
        if ((root["successor_recovery_code"] as? JsonObject)?.keys != CODE_KEYS) invalid()
        val decoded = decode<AccountRecoveryConsumptionResponse>(root)
        validateReplay(status, decoded.replayed)
        try {
            validateDeviceSessionContract(
                decoded.session,
                request.sessionId,
                request.clientInstanceId,
                request.deviceLabel,
                request.clientVersion,
                expectedMinimumRevision = 1,
            )
            if (decoded.session.revision != 1L) invalid()
            validateRecoverySessionTime(decoded.session, receivedAt, preparedAt, decoded.replayed)
        } catch (_: IllegalArgumentException) {
            invalid()
        }
        validateCode(decoded.successorRecoveryCode, receivedAt)
        if (decoded.successorRecoveryCode.id != request.successorRecoveryCodeId) invalid()
        validateMutationTime(
            decoded.successorRecoveryCode.createdAt,
            preparedAt,
            receivedAt,
            decoded.replayed,
        )
        if (decoded.successorRecoveryCode.createdAt != decoded.session.createdAt) invalid()
        return decoded
    }

    private fun requestBuilder(url: String, bearer: String): Request.Builder = Request.Builder()
        .url(url)
        .header("Accept", "application/json")
        .header("Authorization", "Bearer $bearer")
        .header("Cache-Control", "no-store")
        .header("Pragma", "no-cache")

    private suspend fun execute(
        configuration: AuthenticatedApiConfiguration?,
        request: Request,
        expectedStatuses: Set<Int>,
    ): Response {
        val response = configuration?.executeAuthenticated(client, request)
            ?: client.newCall(request).awaitDeviceAuthResponse()
        if (!hasStrictNoStoreHeaders(response)) {
            val failure = if (response.code in expectedStatuses) {
                DeviceAuthApiException.InvalidResponse()
            } else {
                DeviceAuthApiException.Unavailable()
            }
            response.close()
            throw failure
        }
        if (response.code !in expectedStatuses) {
            throw response.use(::decodeTrustedDeviceAuthError)
        }
        if (!hasExactJsonMediaType(response)) {
            response.close()
            throw DeviceAuthApiException.InvalidResponse()
        }
        return response
    }

    private fun decodeRoot(response: Response): Pair<JsonObject, Instant> = response.use {
        val text = readBounded(response)
        val receivedAt = now()
        try {
            if (StrictDeviceAuthJsonScanner(text).hasDuplicateKeys()) invalid()
            val root = DEVICE_AUTH_JSON.parseToJsonElement(text) as? JsonObject ?: invalid()
            root to receivedAt
        } catch (_: SerializationException) {
            invalid()
        } catch (error: DeviceAuthApiException.InvalidResponse) {
            throw error
        } catch (_: IllegalArgumentException) {
            invalid()
        }
    }

    private inline fun <reified T> decode(root: JsonObject): T = try {
        DEVICE_AUTH_JSON.decodeFromJsonElement(root)
    } catch (_: SerializationException) {
        invalid()
    } catch (_: IllegalArgumentException) {
        invalid()
    }

    private fun readBounded(response: Response): String {
        if (response.body.contentLength() > MAX_RECOVERY_RESPONSE_BYTES) invalid()
        val bytes = try {
            response.body.byteStream().use { input ->
                val buffer = ByteArray(MAX_RECOVERY_RESPONSE_BYTES + 1)
                var count = 0
                try {
                    while (count < buffer.size) {
                        val read = input.read(buffer, count, buffer.size - count)
                        if (read < 0) break
                        count += read
                    }
                    if (count > MAX_RECOVERY_RESPONSE_BYTES || input.read() >= 0) invalid()
                    buffer.copyOf(count)
                } finally {
                    buffer.fill(0)
                }
            }
        } catch (error: DeviceAuthApiException) {
            throw error
        } catch (_: IOException) {
            throw DeviceAuthApiException.Unavailable()
        }
        return try {
            decodeStrictUtf8(bytes) ?: invalid()
        } finally {
            bytes.fill(0)
        }
    }

    private fun validateIssueRequest(request: CreateAccountRecoveryCodeRequest) {
        requireCanonicalUuid(request.id)
        validateExactDeviceToken(request.recoveryCode, ACCOUNT_RECOVERY_TOKEN_PREFIX)
        if ((request.replacesRecoveryCodeId == null) !=
            (request.replacesRecoveryCodeRevision == null)
        ) invalid()
        request.replacesRecoveryCodeId?.let {
            requireCanonicalUuid(it)
            if (it == request.id) invalid()
        }
        request.replacesRecoveryCodeRevision?.let { if (it <= 0 || it == Long.MAX_VALUE) invalid() }
    }

    private fun validateConsumeRequest(
        recoveryCode: String,
        request: ConsumeAccountRecoveryCodeRequest,
    ) {
        requireCanonicalUuid(request.sessionId)
        requireCanonicalUuid(request.clientInstanceId)
        requireCanonicalUuid(request.successorRecoveryCodeId)
        if (request.sessionId == request.successorRecoveryCodeId) invalid()
        validateExactDeviceToken(request.accessToken, DEVICE_ACCESS_TOKEN_PREFIX)
        validateExactDeviceToken(request.refreshToken, DEVICE_REFRESH_TOKEN_PREFIX)
        validateExactDeviceToken(request.successorRecoveryCode, ACCOUNT_RECOVERY_TOKEN_PREFIX)
        requireDistinctCredentialMaterials(
            recoveryCode,
            request.accessToken,
            request.refreshToken,
            request.successorRecoveryCode,
        )
        if (
            request.clientKind != "android" ||
            request.clientContractVersion != DEVICE_AUTH_CONTRACT_VERSION ||
            request.clientCapabilities != ANDROID_DEVICE_AUTH_CAPABILITIES
        ) invalid()
        try {
            requireValidDeviceIdentity(request.deviceLabel, request.clientVersion)
        } catch (_: IllegalArgumentException) {
            invalid()
        }
    }

    private fun validateCode(code: AccountRecoveryCodeContract, receivedAt: Instant) {
        requireCanonicalUuid(code.id)
        if (code.revision != 1L) invalid()
        val created = parseInstant(code.createdAt)
        if (created.isAfter(checkedPlus(receivedAt, CLOCK_SKEW))) invalid()
    }

    private fun validateMutationTime(
        createdAt: String,
        preparedAt: Instant,
        receivedAt: Instant,
        replayed: Boolean,
    ) {
        val created = parseInstant(createdAt)
        if (created.isBefore(checkedMinus(preparedAt, CLOCK_SKEW))) invalid()
        if (!replayed && created.isBefore(checkedMinus(receivedAt, CLOCK_SKEW))) invalid()
    }

    private fun validateRecoverySessionTime(
        session: DeviceSessionContract,
        receivedAt: Instant,
        preparedAt: Instant,
        replayed: Boolean,
    ) {
        val issued = parseInstant(session.credentialIssuedAt)
        val lastSeen = parseInstant(session.lastSeenAt)
        val accessExpiry = parseInstant(session.accessExpiresAt)
        val refreshIdleExpiry = parseInstant(session.refreshIdleExpiresAt)
        val absoluteExpiry = parseInstant(session.absoluteExpiresAt)
        if (issued.isBefore(checkedMinus(preparedAt, CLOCK_SKEW))) invalid()
        if (issued.isAfter(checkedPlus(receivedAt, CLOCK_SKEW))) invalid()
        if (lastSeen.isAfter(checkedPlus(receivedAt, CLOCK_SKEW))) invalid()
        requireRecoverySessionExpiriesAfterReceipt(
            refreshIdleExpiry,
            absoluteExpiry,
            receivedAt,
        )
        if (!replayed) {
            if (issued.isBefore(checkedMinus(receivedAt, CLOCK_SKEW))) invalid()
            if (!accessExpiry.isAfter(receivedAt)) invalid()
        }
    }

    private fun validateReplay(status: Int, replayed: Boolean) {
        if ((status == 200) != replayed || (status == 201) == replayed) invalid()
    }

    private fun requireCanonicalUuid(value: String) {
        val parsed = try {
            UUID.fromString(value)
        } catch (_: IllegalArgumentException) {
            invalid()
        }
        if (parsed == UUID(0, 0) || parsed.toString() != value) invalid()
    }

    private fun parseInstant(value: String): Instant = try {
        Instant.parse(value)
    } catch (_: DateTimeParseException) {
        invalid()
    }

    private fun checkedPlus(value: Instant, duration: Duration): Instant = try {
        value.plus(duration)
    } catch (_: DateTimeException) {
        invalid()
    } catch (_: ArithmeticException) {
        invalid()
    }

    private fun checkedMinus(value: Instant, duration: Duration): Instant = try {
        value.minus(duration)
    } catch (_: DateTimeException) {
        invalid()
    } catch (_: ArithmeticException) {
        invalid()
    }

    private fun invalid(): Nothing = throw DeviceAuthApiException.InvalidResponse()

    private companion object {
        val JSON_MEDIA_TYPE = "application/json; charset=utf-8".toMediaType()
        internal const val MAX_RECOVERY_RESPONSE_BYTES = 64 * 1024
        val CLOCK_SKEW: Duration = Duration.ofMinutes(5)
        val CURRENT_RESPONSE_KEYS = setOf("recovery_code")
        val CODE_KEYS = setOf("id", "created_at", "revision")
        val MUTATION_RESPONSE_KEYS = setOf("recovery_code", "replayed")
        val CONSUMPTION_RESPONSE_KEYS = setOf("session", "successor_recovery_code", "replayed")
        val SESSION_KEYS = setOf(
            "id",
            "client_instance_id",
            "client_kind",
            "device_label",
            "scopes",
            "client_contract_version",
            "client_version",
            "client_capabilities",
            "created_at",
            "last_seen_at",
            "credential_issued_at",
            "access_expires_at",
            "refresh_idle_expires_at",
            "absolute_expires_at",
            "revision",
        )
    }
}

internal fun requireRecoverySessionExpiriesAfterReceipt(
    refreshIdleExpiry: Instant,
    absoluteExpiry: Instant,
    receivedAt: Instant,
) {
    require(absoluteExpiry.isAfter(receivedAt))
    require(refreshIdleExpiry.isAfter(receivedAt))
}
