package com.greengolddog.dayweave.network

import java.io.IOException
import java.security.SecureRandom
import java.time.Duration
import java.time.Instant
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

internal enum class DeviceAuthActionResult {
    SUCCESS,
    PENDING_RETRY,
    AUTH_REQUIRED,
    NETWORK_FAILURE,
    SERVER_REJECTED,
    STORAGE_FAILURE,
    CLEANUP_PENDING,
    CACHE_FENCE_BLOCKED,
    STALE_STATE,
    NOT_ALLOWED,
}

private sealed interface BindingDestroyOutcome {
    data object FenceBlocked : BindingDestroyOutcome
    data object Stale : BindingDestroyOutcome
    data class Destroyed(val result: DeviceAuthDestroyResult) : BindingDestroyOutcome
}

internal interface DeviceAuthBindingFence {
    /**
     * Must durably quarantine all API-bound cache before a new binding becomes usable. The
     * coordinator invokes this while holding the process-wide binding writer; implementations
     * must not acquire or release that writer themselves.
     */
    suspend fun beforeBindingChange(
        previousBaseUrl: String?,
        previousBindingId: String?,
        nextBaseUrl: String?,
        nextBindingId: String?,
    ): Boolean
}

internal object AllowDeviceAuthBindingChange : DeviceAuthBindingFence {
    override suspend fun beforeBindingChange(
        previousBaseUrl: String?,
        previousBindingId: String?,
        nextBaseUrl: String?,
        nextBindingId: String?,
    ): Boolean = true
}

internal interface DeviceCredentialGenerator {
    fun sessionId(): String
    fun token(prefix: String): String
}

internal object SecureDeviceCredentialGenerator : DeviceCredentialGenerator {
    private val random = SecureRandom()

    override fun sessionId(): String = UUID.randomUUID().toString()

    override fun token(prefix: String): String {
        val material = ByteArray(32)
        random.nextBytes(material)
        return try {
            prefix + Base64.getUrlEncoder().withoutPadding().encodeToString(material)
        } finally {
            material.fill(0)
        }
    }
}

internal interface DeviceAuthRequestExecutor {
    suspend fun executeAuthenticated(
        configuration: AuthenticatedApiConfiguration,
        client: OkHttpClient,
        request: Request,
    ): Response
}

internal suspend fun AuthenticatedApiConfiguration.executeAuthenticated(
    client: OkHttpClient,
    request: Request,
): Response = deviceAuthExecutor?.executeAuthenticated(this, client, request)
    ?: client.newCall(request).awaitDeviceAuthResponse()

/** Process-wide owner of device enrollment, credential rotation, and authenticated retries. */
internal class DurableDeviceAuthCoordinator(
    private val store: DeviceAuthEnvelopeStore,
    private val transport: DeviceAuthTransport,
    private val clientVersion: String,
    private val deviceLabel: String,
    private val bindingOperationGate: ApiBindingOperationGate = ApiBindingOperationGate(),
    private val bindingFence: DeviceAuthBindingFence = AllowDeviceAuthBindingChange,
    private val now: () -> Instant = Instant::now,
    private val generator: DeviceCredentialGenerator = SecureDeviceCredentialGenerator,
    private val allowCleartextLoopbackForTests: Boolean = false,
    private val proactiveRefreshWindow: Duration = Duration.ofMinutes(2),
) : ApiCredentialStore, DeviceAuthRequestExecutor {
    private val stateMutex = Mutex()
    private val operationScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val inFlightOperations = mutableMapOf<AuthOperationKey, Deferred<DeviceAuthActionResult>>()
    // Keep only the redacted projection in the long-lived flow. Retaining the initial envelope
    // here would also retain its credential strings for the lifetime of the process.
    private val mutableUiState = MutableStateFlow(store.read().state.toUiState())
    val uiState: StateFlow<DeviceAuthUiState> = mutableUiState.asStateFlow()

    init {
        requireValidDeviceIdentity(deviceLabel, clientVersion)
        require(!proactiveRefreshWindow.isNegative && proactiveRefreshWindow < DEVICE_AUTH_ACCESS_TTL)
    }

    override fun snapshot(): ApiConnectionSnapshot {
        val envelope = store.read()
        val state = envelope.state
        updateUiAfterDestroyCleanup(state)
        val usable = state is StoredDeviceAuthState.Active ||
            state is StoredDeviceAuthState.RefreshPending
        return ApiConnectionSnapshot(
            baseUrl = state.baseUrl,
            hasBearerToken = usable,
            lastSuccessfulSyncEpochMillis = store.lastSuccessfulSyncEpochMillis(),
            configurationId = state.bindingId(),
        )
    }

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        val state = store.read().state
        updateUiAfterDestroyCleanup(state)
        val token = when (state) {
            is StoredDeviceAuthState.Active -> state.accessToken.value
            is StoredDeviceAuthState.RefreshPending -> state.currentAccessToken.value
            else -> return null
        }
        val binding = state.bindingId() ?: return null
        return AuthenticatedApiConfiguration.createCoordinated(
            baseUrl = requireNotNull(state.baseUrl),
            bearerToken = token,
            configurationId = binding,
            executor = this,
            bindingGate = bindingOperationGate,
            bindingGeneration = bindingOperationGate.captureGeneration(),
            allowCleartextLoopback = allowCleartextLoopbackForTests,
        )
    }

    /**
     * Compatibility reads remain supported, but writes must use the suspend enrollment flows so
     * binding-cache fencing cannot be bypassed.
     */
    override fun update(baseUrl: String, bearerToken: String?) {
        val normalized = normalizeBaseUrlForDeviceAuth(baseUrl.trim(), allowCleartextLoopbackForTests)
            .toString()
        val current = store.read()
        if (bearerToken == null) {
            if (current.state.baseUrl != normalized) {
                throw InvalidApiConfigurationException(
                    "Changing the API endpoint requires a new bootstrap or enrollment flow",
                )
            }
            return
        }
        throw InvalidApiConfigurationException(
            "Use the reviewed bootstrap-upgrade or one-time enrollment flow",
        )
    }

    /** Destructive local removal must go through [destroyLocalOnly] with explicit confirmation. */
    override fun clear() {
        throw SecureCredentialException(
            "Use revoke-first sign-out or explicitly confirmed local-only destruction",
        )
    }

    override fun recordSuccessfulSync(epochMillis: Long) = store.recordSuccessfulSync(epochMillis)

    suspend fun recoverPendingOrUpgradeLegacy(): DeviceAuthActionResult {
        setBusy("Checking durable authentication…")
        val envelope = store.read()
        val result = when (val state = envelope.state) {
            is StoredDeviceAuthState.Legacy -> upgradeLegacyLocked(envelope, state)
            is StoredDeviceAuthState.EnrollmentCreationPending ->
                completeEnrollmentCreationLocked(state)
            is StoredDeviceAuthState.EnrollmentPending -> completeEnrollmentLocked(state)
            is StoredDeviceAuthState.RefreshPending -> completeRefreshLocked(state)
            else -> DeviceAuthActionResult.SUCCESS
        }
        refreshUi(result.message())
        return result
    }

    suspend fun upgradeWithBootstrap(
        baseUrl: String,
        bootstrapToken: String,
    ): DeviceAuthActionResult {
        setBusy("Upgrading the reviewed hybrid bootstrap…")
        val current = store.read()
        if (!canStartEnrollment(current.state)) {
            refreshUi("Sign out the active device session before replacing authentication.")
            return DeviceAuthActionResult.NOT_ALLOWED
        }
        val normalized = try {
            normalizeBaseUrlForDeviceAuth(baseUrl.trim(), allowCleartextLoopbackForTests).toString()
        } catch (_: IllegalArgumentException) {
            refreshUi("Enter a valid HTTPS DayWeave API endpoint.")
            return DeviceAuthActionResult.SERVER_REJECTED
        }
        if (!canEnrollAtOrigin(current.state, normalized)) {
            refreshUi(
                "Remove local authentication explicitly before changing the origin of a possibly live session.",
            )
            return DeviceAuthActionResult.NOT_ALLOWED
        }
        val token = try {
            validateLegacyBootstrapToken(bootstrapToken)
        } catch (_: IllegalArgumentException) {
            refreshUi("Enter a valid legacy bootstrap credential, not a dw_ device code.")
            return DeviceAuthActionResult.SERVER_REJECTED
        }
        // Bootstrap authority is used only for enrollment creation. In particular, a durable
        // Reauth state never becomes ordinary-authorizable Legacy state if creation is lost or
        // rejected.
        val result = createEnrollmentWithBootstrapLocked(current, normalized, token)
        refreshUi(result.message())
        return result
    }

    suspend fun consumeOneTimeEnrollmentCode(
        baseUrl: String,
        enrollmentCode: String,
    ): DeviceAuthActionResult {
        setBusy("Journaling the one-time enrollment…")
        val current = store.read()
        if (!canStartEnrollment(current.state)) {
            refreshUi("Sign out the active device session before consuming another enrollment.")
            return DeviceAuthActionResult.NOT_ALLOWED
        }
        val normalized = try {
            normalizeBaseUrlForDeviceAuth(baseUrl.trim(), allowCleartextLoopbackForTests).toString()
        } catch (_: IllegalArgumentException) {
            refreshUi("Enter a valid HTTPS DayWeave API endpoint.")
            return DeviceAuthActionResult.SERVER_REJECTED
        }
        if (!canEnrollAtOrigin(current.state, normalized)) {
            refreshUi(
                "Remove local authentication explicitly before changing the origin of a possibly live session.",
            )
            return DeviceAuthActionResult.NOT_ALLOWED
        }
        try {
            validateExactDeviceToken(enrollmentCode, DEVICE_ENROLLMENT_TOKEN_PREFIX)
        } catch (_: IllegalArgumentException) {
            refreshUi("Enter an exact one-time dw_en1_ enrollment code.")
            return DeviceAuthActionResult.SERVER_REJECTED
        }
        val pending = newEnrollmentPending(
            baseUrl = normalized,
            clientInstanceId = current.state.clientInstanceId ?: generator.sessionId(),
            enrollmentToken = enrollmentCode,
            previousBaseUrl = current.state.baseUrl,
            previousBindingId = priorBinding(current.state),
        ) ?: run {
            refreshUi("Secure credential generation failed before enrollment was sent.")
            return DeviceAuthActionResult.STORAGE_FAILURE
        }
        if (!transition(current, pending)) {
            refreshUi("Authentication changed concurrently; the code was not sent.")
            return DeviceAuthActionResult.STALE_STATE
        }
        val result = completeEnrollmentLocked(pending)
        refreshUi(result.message())
        return result
    }

    suspend fun signOutRevokeFirst(): DeviceAuthActionResult {
        setBusy("Revoking this device session…")
        var envelope = store.read()
        if (envelope.state is StoredDeviceAuthState.RefreshPending) {
            val recovered = completeRefreshLocked(envelope.state as StoredDeviceAuthState.RefreshPending)
            if (recovered != DeviceAuthActionResult.SUCCESS) {
                refreshUi("Sign-out could not verify the pending credential rotation; local state was retained.")
                return recovered
            }
            envelope = store.read()
        }
        var active = envelope.state as? StoredDeviceAuthState.Active
        if (active == null) {
            refreshUi("No active device session is available to revoke.")
            return DeviceAuthActionResult.NOT_ALLOWED
        }
        if (shouldRefresh(active.session)) {
            val refreshed = rotateActiveLocked(envelope, active)
            if (refreshed != DeviceAuthActionResult.SUCCESS) {
                refreshUi("Sign-out could not refresh authentication; local state was retained.")
                return refreshed
            }
            envelope = store.read()
            active = envelope.state as? StoredDeviceAuthState.Active ?: run {
                refreshUi("Authentication changed during sign-out; no revoke request was sent.")
                return DeviceAuthActionResult.STALE_STATE
            }
        }
        val firstRevoke = revokeActiveLocked(envelope, active)
        if (firstRevoke == DeviceAuthActionResult.AUTH_REQUIRED) {
            // A current-session DELETE gets one refresh and one retry with the same method, URL,
            // and session id. A second 401 is never interpreted as successful revocation.
            val refreshed = rotateActiveLocked(envelope, active)
            if (refreshed != DeviceAuthActionResult.SUCCESS) {
                refreshUi("Server revocation was not authenticated; local state was retained.")
                return refreshed
            }
            envelope = store.read()
            active = envelope.state as? StoredDeviceAuthState.Active ?: run {
                refreshUi("Authentication changed before the revoke retry; newer state was retained.")
                return DeviceAuthActionResult.STALE_STATE
            }
            val retriedRevoke = revokeActiveLocked(envelope, active)
            if (retriedRevoke == DeviceAuthActionResult.AUTH_REQUIRED) {
                val rejectedEnvelope = store.read()
                if (rejectedEnvelope != envelope || rejectedEnvelope.state != active) {
                    refreshUi("The revoke rejection belonged to a stale credential; newer authentication was retained.")
                    return DeviceAuthActionResult.STALE_STATE
                }
                val quarantined = StoredDeviceAuthState.Reauth(
                    baseUrl = active.baseUrl,
                    clientInstanceId = active.clientInstanceId,
                    previousSessionId = active.session.id,
                    reason = REAUTH_SESSION_REVOKED,
                )
                if (!transition(rejectedEnvelope, quarantined)) {
                    refreshUi("Authentication changed after the revoke rejection; newer state was retained.")
                    return DeviceAuthActionResult.STALE_STATE
                }
                refreshUi(
                    "The exact refreshed credential was rejected during revocation. Server revocation and local deletion were not reported as successful.",
                )
                return DeviceAuthActionResult.AUTH_REQUIRED
            }
            if (retriedRevoke != DeviceAuthActionResult.SUCCESS) {
                refreshUi("Server revocation could not be confirmed; local state was retained.")
                return retriedRevoke
            }
        } else if (firstRevoke != DeviceAuthActionResult.SUCCESS) {
            refreshUi("Server revocation could not be confirmed; local state was retained.")
            return firstRevoke
        }
        val destroyResult = try {
            bindingOperationGate.invalidateBeforeQuarantine {
                if (store.read() != envelope) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.Stale
                }
                if (
                    !bindingFence.beforeBindingChange(
                        active.baseUrl,
                        active.session.id,
                        null,
                        null,
                    )
                ) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.FenceBlocked
                }
                if (store.read() != envelope) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.Stale
                }
                BindingDestroyOutcome.Destroyed(store.destroy(envelope))
            }
        } catch (_: IllegalStateException) {
            refreshUi("The server session was revoked, but local credential removal could not be confirmed.")
            return DeviceAuthActionResult.STORAGE_FAILURE
        }
        val destroyed = when (destroyResult) {
            BindingDestroyOutcome.FenceBlocked -> {
                refreshUi("The server session was revoked, but encrypted cache quarantine failed; local state was retained.")
                return DeviceAuthActionResult.CACHE_FENCE_BLOCKED
            }
            BindingDestroyOutcome.Stale -> {
                refreshUi("The server session was revoked, but newer local authentication was retained.")
                return DeviceAuthActionResult.STALE_STATE
            }
            is BindingDestroyOutcome.Destroyed -> destroyResult.result
        }
        when (destroyed) {
            DeviceAuthDestroyResult.STALE -> {
                refreshUi("The server session was revoked, but newer local authentication was retained.")
                return DeviceAuthActionResult.STALE_STATE
            }
            DeviceAuthDestroyResult.CREDENTIALS_DESTROYED_CLEANUP_PENDING -> {
                mutableUiState.value = destroyCleanupPendingUiState(
                    "The server session was revoked and credentials were removed. Obsolete Keystore cleanup is pending and will retry automatically.",
                )
                return DeviceAuthActionResult.CLEANUP_PENDING
            }
            DeviceAuthDestroyResult.DESTROYED -> Unit
        }
        val reset = store.read()
        mutableUiState.value = reset.state.toUiState(
            overrideMessage = "This device session was revoked and local credentials were removed.",
        )
        return DeviceAuthActionResult.SUCCESS
    }

    suspend fun destroyLocalOnly(confirmed: Boolean): DeviceAuthActionResult {
        if (!confirmed) return DeviceAuthActionResult.NOT_ALLOWED
        setBusy("Removing local authentication only…")
        val envelope = store.read()
        val state = envelope.state
        val destroyResult = try {
            bindingOperationGate.invalidateBeforeQuarantine {
                if (store.read() != envelope) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.Stale
                }
                if (
                    !bindingFence.beforeBindingChange(
                        state.baseUrl,
                        priorBinding(state),
                        null,
                        null,
                    )
                ) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.FenceBlocked
                }
                if (store.read() != envelope) {
                    return@invalidateBeforeQuarantine BindingDestroyOutcome.Stale
                }
                BindingDestroyOutcome.Destroyed(store.destroy(envelope))
            }
        } catch (_: IllegalStateException) {
            refreshUi("Local credential removal could not be confirmed; authentication remains fail-closed.")
            return DeviceAuthActionResult.STORAGE_FAILURE
        }
        val destroyed = when (destroyResult) {
            BindingDestroyOutcome.FenceBlocked -> {
                refreshUi("Encrypted cache quarantine failed; local authentication was retained.")
                return DeviceAuthActionResult.CACHE_FENCE_BLOCKED
            }
            BindingDestroyOutcome.Stale -> {
                refreshUi("Authentication changed concurrently; the newer state was retained.")
                return DeviceAuthActionResult.STALE_STATE
            }
            is BindingDestroyOutcome.Destroyed -> destroyResult.result
        }
        when (destroyed) {
            DeviceAuthDestroyResult.STALE -> {
                refreshUi("Authentication changed concurrently; the newer state was retained.")
                return DeviceAuthActionResult.STALE_STATE
            }
            DeviceAuthDestroyResult.CREDENTIALS_DESTROYED_CLEANUP_PENDING -> {
                mutableUiState.value = destroyCleanupPendingUiState(
                    "Local credentials were removed. Obsolete Keystore cleanup is pending and will retry automatically; server authority may remain active.",
                )
                return DeviceAuthActionResult.CLEANUP_PENDING
            }
            DeviceAuthDestroyResult.DESTROYED -> Unit
        }
        val reset = store.read()
        mutableUiState.value = reset.state.toUiState(
            overrideMessage =
            "Local authentication was removed. A server session or bootstrap authority may still remain active.",
        )
        return DeviceAuthActionResult.SUCCESS
    }

    override suspend fun executeAuthenticated(
        configuration: AuthenticatedApiConfiguration,
        client: OkHttpClient,
        request: Request,
    ): Response {
        if (!isRequestBoundToConfiguration(configuration, request)) {
            throw DeviceAuthenticationChangedException()
        }
        val baseRequest = request.newBuilder().removeHeader("Authorization").build()
        val first = prepareAuthorization(
            expectedBindingId = configuration.configurationId,
            expectedBaseUrl = configuration.baseUrl.toString(),
        )
        val firstRequest = baseRequest.newBuilder()
            .header("Authorization", "Bearer ${first.token}")
            .build()
        // This exact preflight prevents a lease replaced after preparation from being sent. A
        // different process can still replace the envelope after this read and before OkHttp
        // dispatches; the exact post-response check below fences that unavoidable TOCTOU window.
        requireCurrentLease(first)
        val response = client.newCall(firstRequest).awaitDeviceAuthResponse()
        if (
            response.code != 401 ||
            first.isLegacy ||
            baseRequest.body?.isOneShot() == true ||
            baseRequest.body?.isDuplex() == true ||
            !isTrustedDeviceAuthUnauthorized(response)
        ) {
            return requireCurrentLeaseOrClose(first, response)
        }
        response.close()

        val second = refreshAfterUnauthorized(first)
        val retry = baseRequest.newBuilder()
            .header("Authorization", "Bearer ${second.token}")
            .build()
        val retryResponse = executeRetryWhileLeaseIsCurrent(second, client, retry)
        requireCurrentLeaseOrClose(second, retryResponse)
        if (retryResponse.code == 401 && isTrustedDeviceAuthUnauthorized(retryResponse)) {
            markReauthAfterSecondUnauthorized(second)
        }
        return retryResponse
    }

    private suspend fun prepareAuthorization(
        expectedBindingId: String?,
        expectedBaseUrl: String,
    ): AuthorizationLease {
        repeat(MAX_AUTH_STATE_RETRIES) {
            val envelope = store.read()
            when (val state = envelope.state) {
                is StoredDeviceAuthState.EnrollmentPending -> {
                    when (completeEnrollmentLocked(state)) {
                        DeviceAuthActionResult.SUCCESS,
                        DeviceAuthActionResult.STALE_STATE,
                        -> return@repeat
                        else -> throw DeviceAuthenticationRequiredException()
                    }
                }
                is StoredDeviceAuthState.RefreshPending -> {
                    when (completeRefreshLocked(state)) {
                        DeviceAuthActionResult.SUCCESS,
                        DeviceAuthActionResult.STALE_STATE,
                        -> return@repeat
                        else -> throw DeviceAuthenticationRequiredException()
                    }
                }
                is StoredDeviceAuthState.Active -> {
                    if (shouldRefresh(state.session)) {
                        when (rotateActiveLocked(envelope, state)) {
                            DeviceAuthActionResult.SUCCESS,
                            DeviceAuthActionResult.STALE_STATE,
                            -> return@repeat
                            else -> throw DeviceAuthenticationRequiredException()
                        }
                    }
                    val binding = state.session.id
                    if (binding != expectedBindingId || state.baseUrl != expectedBaseUrl) {
                        throw DeviceAuthenticationChangedException()
                    }
                    return authorizationLease(envelope, state)
                }
                // Legacy and creation-pending bootstrap authority are enrollment-only. An
                // ordinary API request can neither send them nor implicitly start creation.
                else -> {
                    if (state.baseUrl != expectedBaseUrl || state.bindingId() != expectedBindingId) {
                        throw DeviceAuthenticationChangedException()
                    }
                    throw DeviceAuthenticationRequiredException()
                }
            }
        }
        throw DeviceAuthenticationChangedException()
    }

    private suspend fun refreshAfterUnauthorized(first: AuthorizationLease): AuthorizationLease {
        repeat(MAX_AUTH_STATE_RETRIES) {
            val envelope = store.read()
            when (val latest = envelope.state) {
                is StoredDeviceAuthState.RefreshPending -> {
                    if (!isRefreshPendingForLease(latest, first)) {
                        throw DeviceAuthenticationChangedException()
                    }
                    when (completeRefreshLocked(latest)) {
                        DeviceAuthActionResult.SUCCESS,
                        DeviceAuthActionResult.STALE_STATE,
                        -> return@repeat
                        else -> throw DeviceAuthenticationRequiredException()
                    }
                }
                is StoredDeviceAuthState.Active -> {
                    if (canAdoptConcurrentRotation(first, envelope, latest)) {
                        return authorizationLease(envelope, latest)
                    }
                    if (!isCurrentLease(envelope, first)) {
                        throw DeviceAuthenticationChangedException()
                    }
                    when (rotateActiveLocked(envelope, latest)) {
                        DeviceAuthActionResult.SUCCESS,
                        DeviceAuthActionResult.STALE_STATE,
                        -> return@repeat
                        else -> throw DeviceAuthenticationRequiredException()
                    }
                }
                else -> throw DeviceAuthenticationChangedException()
            }
        }
        throw DeviceAuthenticationChangedException()
    }

    private fun authorizationLease(
        envelope: StoredDeviceAuthEnvelope,
        active: StoredDeviceAuthState.Active,
    ) = AuthorizationLease(
        active.accessToken.value,
        active.baseUrl,
        active.clientInstanceId,
        active.session.id,
        envelope.revision,
        envelope.storageIdentity,
        credentialRevision = active.session.revision,
        sessionSnapshot = active.session,
        isLegacy = false,
    )

    private fun isRefreshPendingForLease(
        pending: StoredDeviceAuthState.RefreshPending,
        lease: AuthorizationLease,
    ): Boolean = !lease.isLegacy &&
        pending.baseUrl == lease.baseUrl &&
        pending.clientInstanceId == lease.clientInstanceId &&
        pending.session.id == lease.bindingId &&
        pending.session.revision == lease.credentialRevision &&
        pending.currentAccessToken.value == lease.token &&
        pending.session == lease.sessionSnapshot

    private fun canAdoptConcurrentRotation(
        previous: AuthorizationLease,
        envelope: StoredDeviceAuthEnvelope,
        active: StoredDeviceAuthState.Active,
    ): Boolean {
        val priorSession = previous.sessionSnapshot ?: return false
        return !previous.isLegacy &&
            previous.credentialRevision != null &&
            envelope.revision > previous.envelopeRevision &&
            envelope.storageIdentity != previous.storageIdentity &&
            active.baseUrl == previous.baseUrl &&
            active.clientInstanceId == previous.clientInstanceId &&
            active.session.id == previous.bindingId &&
            active.session.revision > previous.credentialRevision &&
            active.accessToken.value != previous.token &&
            sameImmutableSession(priorSession, active.session) &&
            !Instant.parse(active.session.credentialIssuedAt)
                .isBefore(Instant.parse(priorSession.credentialIssuedAt)) &&
            !Instant.parse(active.session.lastSeenAt)
                .isBefore(Instant.parse(priorSession.lastSeenAt))
    }

    private fun sameImmutableSession(
        previous: DeviceSessionContract,
        current: DeviceSessionContract,
    ): Boolean =
        current.id == previous.id &&
            current.clientInstanceId == previous.clientInstanceId &&
            current.clientKind == previous.clientKind &&
            current.deviceLabel == previous.deviceLabel &&
            current.scopes == previous.scopes &&
            current.clientContractVersion == previous.clientContractVersion &&
            current.clientVersion == previous.clientVersion &&
            current.clientCapabilities == previous.clientCapabilities &&
            current.createdAt == previous.createdAt &&
            current.absoluteExpiresAt == previous.absoluteExpiresAt

    private suspend fun executeRetryWhileLeaseIsCurrent(
        lease: AuthorizationLease,
        client: OkHttpClient,
        request: Request,
    ): Response {
        // Never retain the coordinator mutex while awaiting provider I/O. The preflight compares
        // the exact durable envelope identity, not only semantically equivalent session fields.
        requireCurrentLease(lease)
        return client.newCall(request).awaitDeviceAuthResponse()
    }

    private suspend fun requireCurrentLease(lease: AuthorizationLease) {
        val isCurrent = stateMutex.withLock { isCurrentLease(store.read(), lease) }
        if (!isCurrent) throw DeviceAuthenticationChangedException()
    }

    private suspend fun requireCurrentLeaseOrClose(
        lease: AuthorizationLease,
        response: Response,
    ): Response {
        val isCurrent = stateMutex.withLock { isCurrentLease(store.read(), lease) }
        if (!isCurrent) {
            response.close()
            throw DeviceAuthenticationChangedException()
        }
        return response
    }

    private fun isCurrentLease(
        envelope: StoredDeviceAuthEnvelope,
        lease: AuthorizationLease,
    ): Boolean {
        if (
            envelope.revision != lease.envelopeRevision ||
            envelope.storageIdentity != lease.storageIdentity
        ) {
            return false
        }
        return when (val state = envelope.state) {
            is StoredDeviceAuthState.Legacy -> lease.isLegacy &&
                state.baseUrl == lease.baseUrl &&
                state.clientInstanceId == lease.clientInstanceId &&
                state.bindingId == lease.bindingId &&
                state.bootstrapToken.value == lease.token
            is StoredDeviceAuthState.Active -> !lease.isLegacy &&
                state.baseUrl == lease.baseUrl &&
                state.clientInstanceId == lease.clientInstanceId &&
                state.session.id == lease.bindingId &&
                state.session.revision == lease.credentialRevision &&
                state.accessToken.value == lease.token
            else -> false
        }
    }

    private suspend fun markReauthAfterSecondUnauthorized(lease: AuthorizationLease) = stateMutex.withLock {
        val current = store.read()
        if (!isCurrentLease(current, lease)) return@withLock
        val active = current.state as? StoredDeviceAuthState.Active ?: return@withLock
        transition(
            current,
            StoredDeviceAuthState.Reauth(
                baseUrl = active.baseUrl,
                clientInstanceId = active.clientInstanceId,
                previousSessionId = active.session.id,
                reason = REAUTH_REFRESH_REJECTED,
            ),
        )
        refreshUi("The refreshed device credential was rejected. Re-enrollment is required.")
    }

    private suspend fun upgradeLegacyLocked(
        envelope: StoredDeviceAuthEnvelope,
        legacy: StoredDeviceAuthState.Legacy,
    ): DeviceAuthActionResult = createEnrollmentWithBootstrapLocked(
        expected = envelope,
        baseUrl = legacy.baseUrl,
        bootstrapToken = legacy.bootstrapToken.value,
    )

    private suspend fun createEnrollmentWithBootstrapLocked(
        expected: StoredDeviceAuthEnvelope,
        baseUrl: String,
        bootstrapToken: String,
    ): DeviceAuthActionResult {
        val clientInstanceId = expected.state.clientInstanceId ?: generator.sessionId()
        val pending = newEnrollmentCreationPending(
            baseUrl = baseUrl,
            bootstrapToken = bootstrapToken,
            clientInstanceId = clientInstanceId,
            previousBaseUrl = expected.state.baseUrl,
            previousBindingId = priorBinding(expected.state),
        ) ?: return DeviceAuthActionResult.STORAGE_FAILURE
        if (!transition(expected, pending)) return DeviceAuthActionResult.STALE_STATE
        return completeEnrollmentCreationLocked(pending)
    }

    private suspend fun completeEnrollmentCreationLocked(
        pending: StoredDeviceAuthState.EnrollmentCreationPending,
    ): DeviceAuthActionResult {
        val expected = store.read()
        if (expected.state != pending) return DeviceAuthActionResult.STALE_STATE
        return runSingleFlight(expected, AuthOperationType.CREATE_ENROLLMENT) creation@{
        if (store.read() != expected) return@creation DeviceAuthActionResult.STALE_STATE
        try {
            transport.createEnrollment(pending.request)
        } catch (error: CancellationException) {
            throw error
        } catch (_: DeviceAuthApiException.Unavailable) {
            return@creation DeviceAuthActionResult.PENDING_RETRY
        } catch (_: DeviceAuthApiException.Authentication) {
            transitionCreationPendingToReauth(expected, pending, REAUTH_LOCAL_RECOVERY)
            return@creation DeviceAuthActionResult.AUTH_REQUIRED
        } catch (error: DeviceAuthApiException) {
            if (!error.isDeterministicRejection()) return@creation DeviceAuthActionResult.PENDING_RETRY
            transitionCreationPendingToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
            return@creation DeviceAuthActionResult.SERVER_REJECTED
        } catch (_: IOException) {
            return@creation DeviceAuthActionResult.PENDING_RETRY
        } catch (_: IllegalArgumentException) {
            transitionCreationPendingToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
            return@creation DeviceAuthActionResult.SERVER_REJECTED
        }
        val current = store.read()
        if (current != expected) return@creation DeviceAuthActionResult.STALE_STATE
        val consumption = newEnrollmentPending(
            baseUrl = pending.baseUrl,
            clientInstanceId = pending.clientInstanceId,
            enrollmentToken = pending.enrollmentToken.value,
            previousBaseUrl = pending.previousBaseUrl,
            previousBindingId = pending.previousBindingId,
        ) ?: return@creation DeviceAuthActionResult.STORAGE_FAILURE
        if (!transition(current, consumption)) return@creation DeviceAuthActionResult.STALE_STATE
        completeEnrollmentLocked(consumption)
        }
    }

    private suspend fun completeEnrollmentLocked(
        pending: StoredDeviceAuthState.EnrollmentPending,
    ): DeviceAuthActionResult {
        val expected = store.read()
        if (expected.state != pending) return DeviceAuthActionResult.STALE_STATE
        return runSingleFlight(expected, AuthOperationType.CONSUME_ENROLLMENT) consumption@{
        if (store.read() != expected) return@consumption DeviceAuthActionResult.STALE_STATE
        val mutation = try {
            transport.consumeEnrollment(
                pending.baseUrl,
                pending.enrollmentToken.value,
                ConsumeDeviceEnrollmentRequest(
                    pending.sessionId,
                    pending.accessToken.value,
                    pending.refreshToken.value,
                ),
            )
        } catch (error: CancellationException) {
            throw error
        } catch (_: DeviceAuthApiException.Authentication) {
            transitionPendingToReauth(expected, pending, REAUTH_LOCAL_RECOVERY)
            return@consumption DeviceAuthActionResult.AUTH_REQUIRED
        } catch (error: DeviceAuthApiException) {
            if (error.isDeterministicRejection()) {
                transitionPendingToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
                return@consumption DeviceAuthActionResult.SERVER_REJECTED
            }
            return@consumption DeviceAuthActionResult.PENDING_RETRY
        } catch (_: IOException) {
            return@consumption DeviceAuthActionResult.PENDING_RETRY
        }
        val receivedAt = now()
        try {
            validateDeviceSessionContract(
                mutation.session,
                pending.sessionId,
                pending.clientInstanceId,
                pending.deviceLabel,
                pending.clientVersion,
                expectedMinimumRevision = 1,
            )
            if (mutation.session.revision != 1L) throw IllegalArgumentException()
            validateReceivedSession(
                mutation.session,
                mutation.replayed,
                receivedAt,
                Instant.parse(pending.preparedAt),
            )
        } catch (_: IllegalArgumentException) {
            transitionPendingToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
            return@consumption DeviceAuthActionResult.SERVER_REJECTED
        }
        val active = StoredDeviceAuthState.Active(
            baseUrl = pending.baseUrl,
            clientInstanceId = pending.clientInstanceId,
            session = mutation.session,
            accessToken = pending.accessToken,
            refreshToken = pending.refreshToken,
        )
        try {
            bindingOperationGate.invalidateBeforeQuarantine {
                if (store.read() != expected) {
                    return@invalidateBeforeQuarantine DeviceAuthActionResult.STALE_STATE
                }
                if (
                    !bindingFence.beforeBindingChange(
                        pending.previousBaseUrl,
                        pending.previousBindingId,
                        pending.baseUrl,
                        mutation.session.id,
                    )
                ) {
                    return@invalidateBeforeQuarantine DeviceAuthActionResult.CACHE_FENCE_BLOCKED
                }
                val current = store.read()
                if (current != expected) {
                    return@invalidateBeforeQuarantine DeviceAuthActionResult.STALE_STATE
                }
                if (transition(current, active)) {
                    DeviceAuthActionResult.SUCCESS
                } else {
                    DeviceAuthActionResult.STALE_STATE
                }
            }
        } catch (_: IllegalStateException) {
            DeviceAuthActionResult.STORAGE_FAILURE
        }
        }
    }

    private suspend fun rotateActiveLocked(
        activeEnvelope: StoredDeviceAuthEnvelope,
        active: StoredDeviceAuthState.Active,
    ): DeviceAuthActionResult {
        val currentTime = now()
        if (
            !currentTime.isBefore(active.session.refreshIdleExpiry) ||
            !currentTime.isBefore(active.session.absoluteExpiry)
        ) {
            transition(
                activeEnvelope,
                StoredDeviceAuthState.Reauth(
                    active.baseUrl,
                    active.clientInstanceId,
                    active.session.id,
                    REAUTH_REFRESH_REJECTED,
                ),
            )
            return DeviceAuthActionResult.AUTH_REQUIRED
        }
        val nextPair = generateDistinctRefreshPair(active) ?: return DeviceAuthActionResult.STORAGE_FAILURE
        val pending = StoredDeviceAuthState.RefreshPending(
            baseUrl = active.baseUrl,
            clientInstanceId = active.clientInstanceId,
            session = active.session,
            preparedAt = currentTime.toString(),
            currentAccessToken = active.accessToken,
            currentRefreshToken = active.refreshToken,
            nextAccessToken = DeviceAuthSecret(nextPair.accessToken),
            nextRefreshToken = DeviceAuthSecret(nextPair.refreshToken),
        )
        if (!transition(activeEnvelope, pending)) return DeviceAuthActionResult.STALE_STATE
        return completeRefreshLocked(pending)
    }

    private suspend fun completeRefreshLocked(
        pending: StoredDeviceAuthState.RefreshPending,
    ): DeviceAuthActionResult {
        val expected = store.read()
        if (expected.state != pending) return DeviceAuthActionResult.STALE_STATE
        return runSingleFlight(expected, AuthOperationType.REFRESH_SESSION) refresh@{
        if (store.read() != expected) return@refresh DeviceAuthActionResult.STALE_STATE
        val mutation = try {
            transport.refreshSession(
                pending.baseUrl,
                pending.currentRefreshToken.value,
                RefreshDeviceSessionRequest(
                    pending.nextAccessToken.value,
                    pending.nextRefreshToken.value,
                ),
            )
        } catch (error: CancellationException) {
            throw error
        } catch (_: DeviceAuthApiException.Authentication) {
            transitionPendingRefreshToReauth(expected, pending)
            return@refresh DeviceAuthActionResult.AUTH_REQUIRED
        } catch (error: DeviceAuthApiException) {
            if (error.isDeterministicRejection()) {
                transitionPendingRefreshToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
                return@refresh DeviceAuthActionResult.SERVER_REJECTED
            }
            return@refresh DeviceAuthActionResult.PENDING_RETRY
        } catch (_: IOException) {
            return@refresh DeviceAuthActionResult.PENDING_RETRY
        }
        val receivedAt = now()
        try {
            val expectedNextRevision = try {
                Math.addExact(pending.session.revision, 1)
            } catch (_: ArithmeticException) {
                throw IllegalArgumentException("Credential revision cannot advance")
            }
            validateDeviceSessionContract(
                mutation.session,
                pending.session.id,
                pending.clientInstanceId,
                pending.session.deviceLabel,
                pending.session.clientVersion,
                expectedMinimumRevision = expectedNextRevision,
            )
            validateReceivedSession(
                mutation.session,
                mutation.replayed,
                receivedAt,
                Instant.parse(pending.preparedAt),
            )
            if (
                mutation.session.revision != expectedNextRevision ||
                mutation.session.createdAt != pending.session.createdAt ||
                mutation.session.absoluteExpiresAt != pending.session.absoluteExpiresAt ||
                Instant.parse(mutation.session.lastSeenAt)
                    .isBefore(Instant.parse(pending.session.lastSeenAt)) ||
                Instant.parse(mutation.session.credentialIssuedAt)
                    .isBefore(Instant.parse(pending.session.credentialIssuedAt))
            ) {
                throw IllegalArgumentException()
            }
        } catch (_: IllegalArgumentException) {
            transitionPendingRefreshToReauth(expected, pending, REAUTH_CONTRACT_REJECTED)
            return@refresh DeviceAuthActionResult.SERVER_REJECTED
        }
        val current = store.read()
        if (current != expected) return@refresh DeviceAuthActionResult.STALE_STATE
        val active = StoredDeviceAuthState.Active(
            baseUrl = pending.baseUrl,
            clientInstanceId = pending.clientInstanceId,
            session = mutation.session,
            accessToken = pending.nextAccessToken,
            refreshToken = pending.nextRefreshToken,
        )
        if (transition(current, active)) {
            DeviceAuthActionResult.SUCCESS
        } else {
            DeviceAuthActionResult.STALE_STATE
        }
        }
    }

    private fun transitionPendingToReauth(
        expected: StoredDeviceAuthEnvelope,
        pending: StoredDeviceAuthState.EnrollmentPending,
        reason: String,
    ) {
        val current = store.read()
        if (current != expected || current.state != pending) return
        transition(
            current,
            StoredDeviceAuthState.Reauth(
                pending.baseUrl,
                pending.clientInstanceId,
                // A strict-response failure can occur after the server committed this proposed
                // session. Treat its journaled ID as possibly live so origin changes remain
                // blocked until explicit local-only recovery.
                pending.sessionId,
                reason,
            ),
        )
    }

    private fun transitionCreationPendingToReauth(
        expected: StoredDeviceAuthEnvelope,
        pending: StoredDeviceAuthState.EnrollmentCreationPending,
        reason: String,
    ) {
        val current = store.read()
        if (current != expected || current.state != pending) return
        transition(
            current,
            StoredDeviceAuthState.Reauth(
                pending.baseUrl,
                pending.clientInstanceId,
                pending.previousBindingId ?: pending.enrollmentId,
                reason,
            ),
        )
    }

    private fun transitionPendingRefreshToReauth(
        expected: StoredDeviceAuthEnvelope,
        pending: StoredDeviceAuthState.RefreshPending,
        reason: String = REAUTH_REFRESH_REJECTED,
    ) {
        val current = store.read()
        if (current != expected || current.state != pending) return
        transition(
            current,
            StoredDeviceAuthState.Reauth(
                pending.baseUrl,
                pending.clientInstanceId,
                pending.session.id,
                reason,
            ),
        )
    }

    private suspend fun revokeActiveLocked(
        expected: StoredDeviceAuthEnvelope,
        active: StoredDeviceAuthState.Active,
    ): DeviceAuthActionResult {
        if (expected.state != active) return DeviceAuthActionResult.STALE_STATE
        return runSingleFlight(expected, AuthOperationType.REVOKE_SESSION) revoke@{
            if (store.read() != expected) return@revoke DeviceAuthActionResult.STALE_STATE
            try {
                transport.revokeSession(
                    active.baseUrl,
                    active.accessToken.value,
                    active.session.id,
                )
                DeviceAuthActionResult.SUCCESS
            } catch (error: CancellationException) {
                throw error
            } catch (_: DeviceAuthApiException.Authentication) {
                DeviceAuthActionResult.AUTH_REQUIRED
            } catch (_: DeviceAuthApiException.Unavailable) {
                DeviceAuthActionResult.NETWORK_FAILURE
            } catch (_: DeviceAuthApiException) {
                DeviceAuthActionResult.SERVER_REJECTED
            } catch (_: IOException) {
                DeviceAuthActionResult.NETWORK_FAILURE
            }
        }
    }

    private fun newEnrollmentCreationPending(
        baseUrl: String,
        bootstrapToken: String,
        clientInstanceId: String,
        previousBaseUrl: String?,
        previousBindingId: String?,
    ): StoredDeviceAuthState.EnrollmentCreationPending? = runCatching {
        repeat(MAX_CREDENTIAL_GENERATION_ATTEMPTS) {
            val enrollmentId = generator.sessionId()
            val enrollmentToken = generator.token(DEVICE_ENROLLMENT_TOKEN_PREFIX)
            if (
                !isCanonicalUuid(enrollmentId) ||
                enrollmentId == previousBindingId ||
                enrollmentId == clientInstanceId
            ) {
                return@repeat
            }
            validateExactDeviceToken(enrollmentToken, DEVICE_ENROLLMENT_TOKEN_PREFIX)
            val body = CreateDeviceEnrollmentRequest(
                id = enrollmentId,
                enrollmentToken = enrollmentToken,
                clientInstanceId = clientInstanceId,
                deviceLabel = deviceLabel,
                clientVersion = clientVersion,
            )
            val request = buildEnrollmentCreationHttpRequest(
                baseUrl,
                bootstrapToken,
                body,
                allowCleartextLoopbackForTests,
            )
            return@runCatching StoredDeviceAuthState.EnrollmentCreationPending(
                baseUrl = baseUrl,
                clientInstanceId = clientInstanceId,
                previousBaseUrl = previousBaseUrl,
                previousBindingId = previousBindingId,
                enrollmentId = enrollmentId,
                deviceLabel = deviceLabel,
                clientVersion = clientVersion,
                preparedAt = now().toString(),
                scopes = ANDROID_DEVICE_AUTH_SCOPES,
                capabilities = ANDROID_DEVICE_AUTH_CAPABILITIES,
                enrollmentToken = DeviceAuthSecret(enrollmentToken),
                request = request,
            )
        }
        null
    }.getOrNull()

    private fun newEnrollmentPending(
        baseUrl: String,
        clientInstanceId: String,
        enrollmentToken: String,
        previousBaseUrl: String?,
        previousBindingId: String?,
    ): StoredDeviceAuthState.EnrollmentPending? {
        val generated = generateEnrollmentTuple(enrollmentToken, previousBindingId) ?: return null
        return StoredDeviceAuthState.EnrollmentPending(
            baseUrl = baseUrl,
            clientInstanceId = clientInstanceId,
            previousBaseUrl = previousBaseUrl,
            previousBindingId = previousBindingId,
            sessionId = generated.sessionId,
            deviceLabel = deviceLabel,
            clientVersion = clientVersion,
            preparedAt = now().toString(),
            scopes = ANDROID_DEVICE_AUTH_SCOPES,
            capabilities = ANDROID_DEVICE_AUTH_CAPABILITIES,
            enrollmentToken = DeviceAuthSecret(enrollmentToken),
            accessToken = DeviceAuthSecret(generated.accessToken),
            refreshToken = DeviceAuthSecret(generated.refreshToken),
        )
    }

    private fun generateEnrollmentTuple(
        enrollmentToken: String,
        previousBindingId: String?,
    ): GeneratedEnrollmentTuple? = runCatching {
        repeat(MAX_CREDENTIAL_GENERATION_ATTEMPTS) {
            val access = generator.token(DEVICE_ACCESS_TOKEN_PREFIX)
            val refresh = generator.token(DEVICE_REFRESH_TOKEN_PREFIX)
            val sessionId = generator.sessionId()
            validateExactDeviceToken(access, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(refresh, DEVICE_REFRESH_TOKEN_PREFIX)
            if (!isCanonicalUuid(sessionId) || sessionId == previousBindingId) return@repeat
            val materials = listOf(enrollmentToken, access, refresh).map(::tokenMaterial)
            if (materials.distinct().size == materials.size) {
                return@runCatching GeneratedEnrollmentTuple(sessionId, access, refresh)
            }
        }
        null
    }.getOrNull()

    private fun validateReceivedSession(
        session: DeviceSessionContract,
        replayed: Boolean,
        receivedAt: Instant,
        preparedAt: Instant,
    ) {
        val issued = Instant.parse(session.credentialIssuedAt)
        val lastSeen = Instant.parse(session.lastSeenAt)
        val accessExpiry = Instant.parse(session.accessExpiresAt)
        val refreshIdleExpiry = Instant.parse(session.refreshIdleExpiresAt)
        val absoluteExpiry = Instant.parse(session.absoluteExpiresAt)
        require(!issued.isAfter(receivedAt.plus(RECEIVE_CLOCK_SKEW)))
        require(!lastSeen.isAfter(receivedAt.plus(RECEIVE_CLOCK_SKEW)))
        require(!issued.isBefore(preparedAt.minus(RECEIVE_CLOCK_SKEW)))
        require(refreshIdleExpiry.isAfter(receivedAt))
        require(absoluteExpiry.isAfter(receivedAt))
        if (!replayed) {
            require(!issued.isBefore(receivedAt.minus(RECEIVE_CLOCK_SKEW)))
            require(!lastSeen.isBefore(receivedAt.minus(RECEIVE_CLOCK_SKEW)))
            require(accessExpiry.isAfter(receivedAt))
        }
        // Exact server replay deliberately survives access expiry. Once activated, the normal
        // proactive path immediately journals and rotates the recovered pair before API use.
    }

    private fun generateDistinctRefreshPair(
        active: StoredDeviceAuthState.Active,
    ): GeneratedRefreshPair? = runCatching {
        repeat(MAX_CREDENTIAL_GENERATION_ATTEMPTS) {
            val nextAccess = generator.token(DEVICE_ACCESS_TOKEN_PREFIX)
            val nextRefresh = generator.token(DEVICE_REFRESH_TOKEN_PREFIX)
            validateExactDeviceToken(nextAccess, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(nextRefresh, DEVICE_REFRESH_TOKEN_PREFIX)
            val materials = listOf(
                active.accessToken.value,
                active.refreshToken.value,
                nextAccess,
                nextRefresh,
            ).map(::tokenMaterial)
            if (materials.distinct().size == materials.size) {
                return@runCatching GeneratedRefreshPair(nextAccess, nextRefresh)
            }
        }
        null
    }.getOrNull()

    private fun tokenMaterial(token: String): String = when {
        token.startsWith(DEVICE_ACCESS_TOKEN_PREFIX) -> token.removePrefix(DEVICE_ACCESS_TOKEN_PREFIX)
        token.startsWith(DEVICE_REFRESH_TOKEN_PREFIX) -> token.removePrefix(DEVICE_REFRESH_TOKEN_PREFIX)
        token.startsWith(DEVICE_ENROLLMENT_TOKEN_PREFIX) -> token.removePrefix(DEVICE_ENROLLMENT_TOKEN_PREFIX)
        else -> throw IllegalArgumentException("Invalid device credential")
    }

    private fun isCanonicalUuid(value: String): Boolean =
        runCatching { UUID.fromString(value).toString() == value.lowercase() }.getOrDefault(false)

    private fun isRequestBoundToConfiguration(
        configuration: AuthenticatedApiConfiguration,
        request: Request,
    ): Boolean {
        val base = configuration.baseUrl
        val target = request.url
        return target.scheme == base.scheme &&
            target.host == base.host &&
            target.port == base.port &&
            target.username.isEmpty() &&
            target.password.isEmpty() &&
            target.encodedPath.startsWith(base.encodedPath)
    }

    private fun transition(
        expected: StoredDeviceAuthEnvelope,
        next: StoredDeviceAuthState,
    ): Boolean {
        return try {
            val nextRevision = Math.addExact(expected.revision, 1)
            if (nextRevision >= Long.MAX_VALUE) return false
            if (!store.compareAndSet(expected, next)) return false
            val readback = store.read()
            readback.revision == nextRevision && readback.state == next
        } catch (_: ArithmeticException) {
            false
        } catch (_: IllegalStateException) {
            false
        }
    }

    private suspend fun runSingleFlight(
        expected: StoredDeviceAuthEnvelope,
        operationType: AuthOperationType,
        operation: suspend () -> DeviceAuthActionResult,
    ): DeviceAuthActionResult {
        val identity = expected.storageIdentity ?: return DeviceAuthActionResult.STORAGE_FAILURE
        val key = AuthOperationKey(operationType, expected.revision, identity)
        val deferred = stateMutex.withLock {
            inFlightOperations[key] ?: operationScope.async(
                start = CoroutineStart.LAZY,
            ) {
                operation()
            }.also { created ->
                inFlightOperations[key] = created
                created.invokeOnCompletion {
                    operationScope.launch {
                        stateMutex.withLock {
                            if (inFlightOperations[key] === created) {
                                inFlightOperations.remove(key)
                            }
                        }
                    }
                }
            }
        }
        deferred.start()
        return deferred.await()
    }

    private fun shouldRefresh(session: DeviceSessionContract): Boolean = try {
        !now().isBefore(session.accessExpiry.minus(proactiveRefreshWindow))
    } catch (_: RuntimeException) {
        // An arithmetically unusable durable timestamp must never authorize an API request.
        true
    }

    private fun canStartEnrollment(state: StoredDeviceAuthState): Boolean =
        state is StoredDeviceAuthState.Unconfigured ||
            state is StoredDeviceAuthState.Legacy ||
            state is StoredDeviceAuthState.Reauth

    private fun canEnrollAtOrigin(state: StoredDeviceAuthState, normalizedBaseUrl: String): Boolean =
        state !is StoredDeviceAuthState.Reauth ||
            state.previousSessionId == null ||
            state.baseUrl == normalizedBaseUrl

    private fun priorBinding(state: StoredDeviceAuthState): String? = when (state) {
        is StoredDeviceAuthState.Reauth -> state.previousSessionId
        is StoredDeviceAuthState.EnrollmentCreationPending ->
            state.previousBindingId ?: state.enrollmentId
        else -> state.bindingId()
    }

    private fun setBusy(message: String) {
        mutableUiState.value = store.read().state.toUiState(isBusy = true, overrideMessage = message)
    }

    private fun refreshUi(message: String? = null) {
        mutableUiState.value = store.read().state.toUiState(overrideMessage = message)
    }

    private fun destroyCleanupPendingUiState(message: String) = DeviceAuthUiState(
        phase = DeviceAuthPhase.INCOMPATIBLE,
        baseUrl = null,
        clientInstanceId = null,
        sessionId = null,
        deviceLabel = null,
        accessExpiresAt = null,
        message = message,
    )

    private fun updateUiAfterDestroyCleanup(state: StoredDeviceAuthState) {
        val current = mutableUiState.value
        if (
            current.phase == DeviceAuthPhase.INCOMPATIBLE &&
            state is StoredDeviceAuthState.Unconfigured
        ) {
            mutableUiState.value = state.toUiState(
                overrideMessage = "Secure-storage cleanup finished; authentication is unconfigured.",
            )
        }
    }

    private data class AuthorizationLease(
        val token: String,
        val baseUrl: String,
        val clientInstanceId: String,
        val bindingId: String,
        val envelopeRevision: Long,
        val storageIdentity: DeviceAuthStorageIdentity?,
        val credentialRevision: Long?,
        val sessionSnapshot: DeviceSessionContract?,
        val isLegacy: Boolean,
    ) {
        override fun toString(): String =
            "AuthorizationLease(token=<redacted>, baseUrl=$baseUrl, clientInstanceId=$clientInstanceId, " +
                "bindingId=$bindingId, envelopeRevision=$envelopeRevision)"
    }

    private class AuthOperationKey(
        private val type: AuthOperationType,
        private val envelopeRevision: Long,
        private val storageIdentity: DeviceAuthStorageIdentity,
    ) {
        override fun equals(other: Any?): Boolean =
            other is AuthOperationKey && type == other.type &&
                envelopeRevision == other.envelopeRevision &&
                storageIdentity == other.storageIdentity

        override fun hashCode(): Int =
            31 * (31 * type.hashCode() + envelopeRevision.hashCode()) + storageIdentity.hashCode()

        override fun toString(): String = "AuthOperationKey(<redacted>)"
    }

    private enum class AuthOperationType {
        CREATE_ENROLLMENT,
        CONSUME_ENROLLMENT,
        REFRESH_SESSION,
        REVOKE_SESSION,
    }

    private data class GeneratedEnrollmentTuple(
        val sessionId: String,
        val accessToken: String,
        val refreshToken: String,
    ) {
        override fun toString(): String =
            "GeneratedEnrollmentTuple(sessionId=$sessionId, accessToken=<redacted>, refreshToken=<redacted>)"
    }

    private data class GeneratedRefreshPair(
        val accessToken: String,
        val refreshToken: String,
    ) {
        override fun toString(): String =
            "GeneratedRefreshPair(accessToken=<redacted>, refreshToken=<redacted>)"
    }

    private companion object {
        const val MAX_CREDENTIAL_GENERATION_ATTEMPTS = 8
        const val MAX_AUTH_STATE_RETRIES = 8
        val RECEIVE_CLOCK_SKEW: Duration = Duration.ofMinutes(5)
    }
}

internal class DeviceAuthenticationRequiredException :
    IOException("Durable device authentication is required")

internal class DeviceAuthenticationChangedException :
    IOException("Device authentication changed while the request was in flight")

private fun DeviceAuthActionResult.message(): String = when (this) {
    DeviceAuthActionResult.SUCCESS -> "Durable device authentication is ready."
    DeviceAuthActionResult.PENDING_RETRY ->
        "The exact enrollment or refresh tuple is safely journaled and will retry."
    DeviceAuthActionResult.AUTH_REQUIRED -> "Re-enrollment is required."
    DeviceAuthActionResult.NETWORK_FAILURE ->
        "The network operation was not confirmed; durable local state was retained."
    DeviceAuthActionResult.SERVER_REJECTED ->
        "The server response did not match the Android device-auth contract."
    DeviceAuthActionResult.STORAGE_FAILURE ->
        "Encrypted authentication state could not be advanced."
    DeviceAuthActionResult.CLEANUP_PENDING ->
        "Credentials were removed; obsolete secure-storage cleanup will retry automatically."
    DeviceAuthActionResult.CACHE_FENCE_BLOCKED ->
        "Encrypted API-bound cache could not be quarantined; credentials were not activated."
    DeviceAuthActionResult.STALE_STATE ->
        "Authentication changed concurrently; the newer state was retained."
    DeviceAuthActionResult.NOT_ALLOWED ->
        "This action is not allowed for the current authentication state."
}
