package com.greengolddog.dayweave.network

import java.io.IOException
import java.time.Duration
import java.time.Instant
import java.util.ArrayDeque
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.decodeFromString

internal fun syntheticDeviceToken(prefix: String, marker: Int): String {
    val bytes = ByteArray(32) { index -> (marker * 37 + index * 11).toByte() }
    return try {
        prefix + Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
    } finally {
        bytes.fill(0)
    }
}

internal fun syntheticSession(
    now: Instant,
    id: String = "11111111-1111-4111-8111-111111111111",
    clientInstanceId: String = "22222222-2222-4222-8222-222222222222",
    revision: Long = 1,
    createdAt: Instant = now,
    issuedAt: Instant = now,
    lastSeenAt: Instant = issuedAt,
    accessExpiresAt: Instant = issuedAt.plus(DEVICE_AUTH_ACCESS_TTL),
    refreshIdleExpiresAt: Instant = issuedAt.plus(DEVICE_AUTH_REFRESH_IDLE_TTL),
    absoluteExpiresAt: Instant = createdAt.plus(DEVICE_AUTH_ABSOLUTE_TTL),
) = DeviceSessionContract(
    id = id,
    clientInstanceId = clientInstanceId,
    clientKind = "android",
    deviceLabel = SYNTHETIC_DEVICE_LABEL,
    scopes = ANDROID_DEVICE_AUTH_SCOPES,
    clientContractVersion = DEVICE_AUTH_CONTRACT_VERSION,
    clientVersion = SYNTHETIC_CLIENT_VERSION,
    clientCapabilities = ANDROID_DEVICE_AUTH_CAPABILITIES,
    createdAt = createdAt.toString(),
    lastSeenAt = lastSeenAt.toString(),
    credentialIssuedAt = issuedAt.toString(),
    accessExpiresAt = accessExpiresAt.toString(),
    refreshIdleExpiresAt = refreshIdleExpiresAt.toString(),
    absoluteExpiresAt = absoluteExpiresAt.toString(),
    revision = revision,
)

internal fun syntheticActiveState(
    now: Instant,
    baseUrl: String = SYNTHETIC_BASE_URL,
    clientInstanceId: String = SYNTHETIC_CLIENT_INSTANCE_ID,
    session: DeviceSessionContract = syntheticSession(now, clientInstanceId = clientInstanceId),
    accessMarker: Int = 10,
    refreshMarker: Int = 11,
) = StoredDeviceAuthState.Active(
    baseUrl = baseUrl,
    clientInstanceId = clientInstanceId,
    session = session,
    accessToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, accessMarker)),
    refreshToken = DeviceAuthSecret(syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, refreshMarker)),
)

internal class FakeDeviceAuthEnvelopeStore(
    initialState: StoredDeviceAuthState,
    initialRecoveryJournal: StoredAccountRecoveryJournal? = null,
) :
    DeviceAuthEnvelopeStore {
    private var identityMarker = 1
    var envelope = StoredDeviceAuthEnvelope(
        revision = 1,
        state = initialState,
        accountRecoveryJournal = initialRecoveryJournal,
        storageIdentity = nextIdentity(),
    )
        private set
    var failNextCompareAndSet = false
    var failNextDestroy = false
    var leaveDestroyCleanupPending = false
    var successfulSync: Long? = null
    var beforeRead: (() -> Unit)? = null
    var afterRead: ((StoredDeviceAuthEnvelope) -> Unit)? = null

    override fun read(): StoredDeviceAuthEnvelope {
        beforeRead?.invoke()
        val result = envelope
        afterRead?.invoke(result)
        return result
    }

    override fun compareAndSet(
        expected: StoredDeviceAuthEnvelope,
        nextState: StoredDeviceAuthState,
        nextAccountRecoveryJournal: StoredAccountRecoveryJournal?,
    ): Boolean {
        if (failNextCompareAndSet) {
            failNextCompareAndSet = false
            return false
        }
        if (envelope != expected) return false
        envelope = StoredDeviceAuthEnvelope(
            revision = expected.revision + 1,
            state = nextState,
            accountRecoveryJournal = nextAccountRecoveryJournal,
            storageIdentity = nextIdentity(),
        )
        return true
    }

    override fun destroy(expected: StoredDeviceAuthEnvelope): DeviceAuthDestroyResult {
        if (failNextDestroy) {
            failNextDestroy = false
            return DeviceAuthDestroyResult.STALE
        }
        if (envelope != expected) return DeviceAuthDestroyResult.STALE
        if (leaveDestroyCleanupPending) {
            envelope = StoredDeviceAuthEnvelope(
                revision = 0,
                state = StoredDeviceAuthState.Incompatible("local_destroy_cleanup_pending"),
                storageIdentity = nextIdentity(),
            )
            return DeviceAuthDestroyResult.CREDENTIALS_DESTROYED_CLEANUP_PENDING
        }
        envelope = StoredDeviceAuthEnvelope(
            revision = expected.revision + 1,
            state = StoredDeviceAuthState.Unconfigured(
                baseUrl = null,
                clientInstanceId = "99999999-9999-4999-8999-999999999999",
            ),
            storageIdentity = nextIdentity(),
        )
        return DeviceAuthDestroyResult.DESTROYED
    }

    override fun lastSuccessfulSyncEpochMillis(): Long? = successfulSync

    override fun recordSuccessfulSync(epochMillis: Long) {
        successfulSync = epochMillis
    }

    fun forceState(state: StoredDeviceAuthState, revision: Long = envelope.revision + 1) {
        envelope = StoredDeviceAuthEnvelope(
            revision = revision,
            state = state,
            storageIdentity = nextIdentity(),
        )
    }

    fun forceRecoveryJournal(journal: StoredAccountRecoveryJournal?) {
        envelope = envelope.copy(
            revision = envelope.revision + 1,
            accountRecoveryJournal = journal,
            storageIdentity = nextIdentity(),
        )
    }

    fun forceExactIdentityChange() {
        envelope = envelope.copy(storageIdentity = nextIdentity())
    }

    private fun nextIdentity(): DeviceAuthStorageIdentity =
        DeviceAuthStorageIdentity(byteArrayOf((identityMarker++ and 0xff).toByte()))
}

internal class QueueDeviceCredentialGenerator : DeviceCredentialGenerator {
    private val tokens = ArrayDeque<String>()
    private val sessionIds = ArrayDeque<String>()
    private var tokenMarker = 100
    private var sessionMarker = 100

    fun enqueueToken(token: String) {
        tokens.addLast(token)
    }

    fun enqueueSessionId(sessionId: String) {
        sessionIds.addLast(sessionId)
    }

    override fun sessionId(): String = if (sessionIds.isEmpty()) {
        UUID(0x4000L, 0x8000L + sessionMarker++).toString()
    } else {
        sessionIds.removeFirst()
    }

    override fun token(prefix: String): String = if (tokens.isEmpty()) {
        syntheticDeviceToken(prefix, tokenMarker++)
    } else {
        val queued = tokens.removeFirst()
        prefix + queued.substringAfterDeviceTokenPrefix()
    }
}

private fun String.substringAfterDeviceTokenPrefix(): String = when {
    startsWith(DEVICE_ACCESS_TOKEN_PREFIX) -> removePrefix(DEVICE_ACCESS_TOKEN_PREFIX)
    startsWith(DEVICE_REFRESH_TOKEN_PREFIX) -> removePrefix(DEVICE_REFRESH_TOKEN_PREFIX)
    startsWith(DEVICE_ENROLLMENT_TOKEN_PREFIX) -> removePrefix(DEVICE_ENROLLMENT_TOKEN_PREFIX)
    startsWith(ACCOUNT_RECOVERY_TOKEN_PREFIX) -> removePrefix(ACCOUNT_RECOVERY_TOKEN_PREFIX)
    else -> this
}

internal class RecordingDeviceAuthTransport : DeviceAuthTransport {
    val createCalls = mutableListOf<CreateCall>()
    val consumeCalls = mutableListOf<ConsumeCall>()
    val refreshCalls = mutableListOf<RefreshCall>()
    val revokeCalls = mutableListOf<RevokeCall>()

    var createHandler: suspend (CreateCall) -> DeviceEnrollmentIssuedResponse = {
        throw IOException("synthetic create response not configured")
    }
    var consumeHandler: suspend (ConsumeCall) -> DeviceSessionMutationResponse = {
        throw IOException("synthetic consume response not configured")
    }
    var refreshHandler: suspend (RefreshCall) -> DeviceSessionMutationResponse = {
        throw IOException("synthetic refresh response not configured")
    }
    var revokeHandler: suspend (RevokeCall) -> Unit = {
        throw IOException("synthetic revoke response not configured")
    }

    override suspend fun createEnrollment(
        request: DeviceEnrollmentCreationHttpRequest,
    ): DeviceEnrollmentIssuedResponse {
        val call = CreateCall(request)
        createCalls += call
        return createHandler(call)
    }

    override suspend fun consumeEnrollment(
        baseUrl: String,
        enrollmentToken: String,
        request: ConsumeDeviceEnrollmentRequest,
    ): DeviceSessionMutationResponse {
        val call = ConsumeCall(baseUrl, enrollmentToken, request)
        consumeCalls += call
        return consumeHandler(call)
    }

    override suspend fun refreshSession(
        baseUrl: String,
        refreshToken: String,
        request: RefreshDeviceSessionRequest,
    ): DeviceSessionMutationResponse {
        val call = RefreshCall(baseUrl, refreshToken, request)
        refreshCalls += call
        return refreshHandler(call)
    }

    override suspend fun revokeSession(baseUrl: String, accessToken: String, sessionId: String) {
        val call = RevokeCall(baseUrl, accessToken, sessionId)
        revokeCalls += call
        revokeHandler(call)
    }
}

internal class RecordingDeviceSessionsTransport : DeviceSessionsTransport {
    val listCalls = mutableListOf<AuthenticatedApiConfiguration>()
    val revokeCalls = mutableListOf<Pair<AuthenticatedApiConfiguration, String>>()

    var listHandler: suspend (AuthenticatedApiConfiguration) -> DeviceSessionListResponse = {
        throw IOException("synthetic device-session list response not configured")
    }
    var revokeHandler: suspend (AuthenticatedApiConfiguration, String) -> Unit = { _, _ ->
        throw IOException("synthetic device-session revoke response not configured")
    }

    override suspend fun listSessions(
        configuration: AuthenticatedApiConfiguration,
    ): DeviceSessionListResponse {
        listCalls += configuration
        return listHandler(configuration)
    }

    override suspend fun revokeSession(
        configuration: AuthenticatedApiConfiguration,
        sessionId: String,
    ) {
        revokeCalls += configuration to sessionId
        revokeHandler(configuration, sessionId)
    }
}

internal class CreateCall(
    val journal: DeviceEnrollmentCreationHttpRequest,
) {
    val bootstrapToken: String = journal.authorizationHeader.value.removePrefix("Bearer ")
    val request: CreateDeviceEnrollmentRequest = DEVICE_AUTH_JSON.decodeFromString(
        String(
            Base64.getUrlDecoder().decode(journal.bodyBase64Url.value),
            Charsets.UTF_8,
        ),
    )
    val baseUrl: String = journal.url.removeSuffix("v1/auth/device-enrollments")

    override fun toString(): String = "CreateCall(baseUrl=$baseUrl, bootstrapToken=<redacted>)"
}

internal class ConsumeCall(
    val baseUrl: String,
    val enrollmentToken: String,
    val request: ConsumeDeviceEnrollmentRequest,
) {
    override fun toString(): String = "ConsumeCall(baseUrl=$baseUrl, enrollmentToken=<redacted>)"
}

internal class RefreshCall(
    val baseUrl: String,
    val refreshToken: String,
    val request: RefreshDeviceSessionRequest,
) {
    override fun toString(): String = "RefreshCall(baseUrl=$baseUrl, refreshToken=<redacted>)"
}

internal class RevokeCall(
    val baseUrl: String,
    val accessToken: String,
    val sessionId: String,
) {
    override fun toString(): String =
        "RevokeCall(baseUrl=$baseUrl, accessToken=<redacted>, sessionId=$sessionId)"
}

internal class RecordingDeviceAuthFence(var allowed: Boolean = true) : DeviceAuthBindingFence {
    val calls = mutableListOf<List<String?>>()
    val accountRecoveryPreflightCalls = mutableListOf<List<String?>>()
    var accountRecoveryPreflightAllowed: Boolean = true

    override suspend fun beforeAccountRecoveryRequest(
        previousBaseUrl: String?,
        previousBindingId: String?,
        nextBaseUrl: String,
    ): Boolean {
        accountRecoveryPreflightCalls += listOf(previousBaseUrl, previousBindingId, nextBaseUrl)
        return accountRecoveryPreflightAllowed
    }

    override suspend fun beforeBindingChange(
        previousBaseUrl: String?,
        previousBindingId: String?,
        nextBaseUrl: String?,
        nextBindingId: String?,
    ): Boolean {
        calls += listOf(previousBaseUrl, previousBindingId, nextBaseUrl, nextBindingId)
        return allowed
    }
}

internal const val SYNTHETIC_BASE_URL = "https://api.example.test/tenant/"
internal const val SYNTHETIC_CLIENT_INSTANCE_ID = "22222222-2222-4222-8222-222222222222"
internal const val SYNTHETIC_SESSION_ID = "11111111-1111-4111-8111-111111111111"
internal const val SYNTHETIC_DEVICE_LABEL = "Synthetic Android Device"
internal const val SYNTHETIC_CLIENT_VERSION = "0.1-test"
