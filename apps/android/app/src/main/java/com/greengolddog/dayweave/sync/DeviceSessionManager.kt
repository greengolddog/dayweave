package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.DeviceAuthApiException
import com.greengolddog.dayweave.network.DeviceAuthenticationChangedException
import com.greengolddog.dayweave.network.DeviceAuthenticationRequiredException
import com.greengolddog.dayweave.network.DeviceSessionContract
import com.greengolddog.dayweave.network.DeviceSessionDeleteOutcomeAmbiguousException
import com.greengolddog.dayweave.network.DeviceSessionListResponse
import com.greengolddog.dayweave.network.DeviceSessionsTransport
import com.greengolddog.dayweave.network.MAX_ACTIVE_DEVICE_SESSIONS
import com.greengolddog.dayweave.network.validateListedDeviceSession
import com.greengolddog.dayweave.network.validateListedDeviceSessionOrder
import java.io.IOException
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class DeviceSessionsPhase {
    NOT_CONFIGURED,
    LOADING,
    READY,
    STALE,
    OFFLINE,
    AUTH_REQUIRED,
    ERROR,
}

data class DeviceSessionSummary(
    val id: String,
    val clientKind: String,
    val deviceLabel: String,
    val clientVersion: String,
    val createdAt: Instant,
    val lastSeenAt: Instant,
    val refreshIdleExpiresAt: Instant,
    val absoluteExpiresAt: Instant,
    val revision: Long,
    val isCurrent: Boolean,
)

data class DeviceSessionsState(
    val phase: DeviceSessionsPhase,
    val sessions: List<DeviceSessionSummary> = emptyList(),
    val lastRefreshedAt: Instant? = null,
    val message: String,
    val isBusy: Boolean = false,
    val revokingSessionId: String? = null,
    /** Opaque current device-session binding that owns every row in [sessions]. */
    val configurationId: String? = null,
    /** Durable local device identity paired with [configurationId]. */
    val clientInstanceId: String? = null,
    /** Capability derived only from the exact current row, never from a target row. */
    val currentSessionCanRevoke: Boolean = false,
) {
    val canRevokeRemoteSessions: Boolean
        get() = phase == DeviceSessionsPhase.READY && !isBusy && currentSessionCanRevoke
}

/** One-presentation capability for the exact remote row the owner reviewed. */
class DeviceSessionRevocationConfirmation internal constructor(
    internal val presentationGeneration: Long,
    internal val binding: ApiConnectionSnapshot,
    internal val sessionId: String,
    internal val sessionRevision: Long,
) {
    override fun toString(): String = "DeviceSessionRevocationConfirmation(<redacted>)"
}

/** Foreground-only, memory-only owner inventory for revocable DayWeave device sessions. */
class DeviceSessionManager internal constructor(
    private val credentialStore: ApiCredentialStore,
    private val transport: DeviceSessionsTransport,
    private val now: () -> Instant = Instant::now,
    private val operationAllowed: () -> Boolean = { true },
) {
    private val operationMutex = Mutex()
    private val presentationFence = Any()
    private var presentationGeneration = 1L
    private val activeOperationJobs = mutableMapOf<Job, Int>()
    private val mutableState = MutableStateFlow(
        initialState(visibleBinding()),
    )
    val state: StateFlow<DeviceSessionsState> = mutableState.asStateFlow()

    /** Removes all device labels immediately and prevents any older callback from restoring them. */
    fun quarantineBindingState() {
        val jobs = synchronized(presentationFence) {
            presentationGeneration = nextGeneration(presentationGeneration)
            mutableState.value = initialState(QUARANTINED_BINDING)
            activeOperationJobs.keys.toList().also { activeOperationJobs.clear() }
        }
        jobs.forEach { job ->
            job.cancel(CancellationException("Device-session inventory crossed a privacy boundary"))
        }
    }

    suspend fun refresh() {
        withPresentationOperation { generation ->
            operationMutex.withLock {
                if (!presentationCurrent(generation)) return@withLock
                val binding = credentialStore.snapshot()
                val configuration = resolveConfiguration(binding, generation) ?: return@withLock
                val ticket = try {
                    configuration.beginBindingOperation()
                } catch (_: ApiBindingChangedException) {
                    invalidateForCurrentBinding()
                    return@withLock
                }
                try {
                    if (!operationCurrent(binding, generation)) return@withLock
                    val previous = stateForBinding(binding, generation)
                    if (!publish(
                            binding,
                            generation,
                            previous.copy(
                                phase = DeviceSessionsPhase.LOADING,
                                isBusy = true,
                                revokingSessionId = null,
                                message = "Checking active devices…",
                            ),
                        )
                    ) {
                        return@withLock
                    }
                    try {
                        val listed = mapResponse(transport.listSessions(configuration), binding)
                        publish(
                            binding,
                            generation,
                            listed,
                            advancePresentation = true,
                        )
                    } catch (error: CancellationException) {
                        publish(binding, generation, previous)
                        throw error
                    } catch (error: Exception) {
                        publish(binding, generation, failureState(error, previous))
                    }
                } finally {
                    ticket.release()
                }
            }
        }
    }

    fun revocationConfirmation(sessionId: String): DeviceSessionRevocationConfirmation? {
        val binding = credentialStore.snapshot()
        return synchronized(presentationFence) {
            val current = mutableState.value
            if (
                !operationAllowed() || !current.canRevokeRemoteSessions ||
                current.configurationId == null || current.configurationId != binding.configurationId ||
                current.clientInstanceId == null ||
                current.clientInstanceId != binding.clientInstanceId || !binding.hasBearerToken
            ) {
                return@synchronized null
            }
            val session = current.sessions.singleOrNull { it.id == sessionId }
                ?: return@synchronized null
            if (session.isCurrent || session.id == binding.configurationId) {
                return@synchronized null
            }
            DeviceSessionRevocationConfirmation(
                presentationGeneration = presentationGeneration,
                binding = binding,
                sessionId = session.id,
                sessionRevision = session.revision,
            )
        }
    }

    suspend fun revokeRemote(
        confirmation: DeviceSessionRevocationConfirmation,
    ): Boolean = withPresentationOperation(confirmation.presentationGeneration) { generation ->
        operationMutex.withLock {
            val binding = credentialStore.snapshot()
            val currentAndTarget = synchronized(presentationFence) {
                val displayed = mutableState.value
                val target = displayed.sessions.singleOrNull { it.id == confirmation.sessionId }
                if (
                    operationAllowed() && presentationGeneration == generation &&
                    binding == confirmation.binding && binding.hasBearerToken &&
                    displayed.canRevokeRemoteSessions &&
                    displayed.configurationId == binding.configurationId && target != null &&
                    displayed.clientInstanceId == binding.clientInstanceId &&
                    target.id != binding.configurationId && !target.isCurrent &&
                    target.revision == confirmation.sessionRevision
                ) {
                    displayed to target
                } else {
                    null
                }
            } ?: return@withLock false
            val (current, target) = currentAndTarget
            val configuration = resolveConfiguration(binding, generation) ?: return@withLock false
            val ticket = try {
                configuration.beginBindingOperation()
            } catch (_: ApiBindingChangedException) {
                invalidateForCurrentBinding()
                return@withLock false
            }
            try {
                if (!operationCurrent(binding, generation)) return@withLock false
                if (!publish(
                        binding,
                        generation,
                        current.copy(
                            phase = DeviceSessionsPhase.LOADING,
                            isBusy = true,
                            revokingSessionId = target.id,
                            message = "Revoking ${target.deviceLabel}…",
                        ),
                    )
                ) {
                    return@withLock false
                }
                try {
                    transport.revokeSession(configuration, target.id)
                    reconcileAfterDelete(
                        configuration = configuration,
                        binding = binding,
                        generation = generation,
                        previous = current,
                        target = target,
                        deleteConfirmed = true,
                    )
                } catch (error: CancellationException) {
                    // Cancellation can race a committed DELETE. Keep the old row only as stale;
                    // the next unlocked refresh authoritatively resolves it.
                    publish(
                        binding,
                        generation,
                        current.copy(
                            phase = DeviceSessionsPhase.STALE,
                            isBusy = false,
                            revokingSessionId = null,
                            message = "Revocation outcome is unconfirmed · refresh active devices",
                        ),
                    )
                    throw error
                } catch (error: Exception) {
                    if (error.isAmbiguousOrMissingDelete()) {
                        reconcileAfterDelete(
                            configuration = configuration,
                            binding = binding,
                            generation = generation,
                            previous = current,
                            target = target,
                            deleteConfirmed = false,
                        )
                    } else {
                        publish(binding, generation, failureState(error, current))
                        false
                    }
                }
            } finally {
                ticket.release()
            }
        }
    } ?: false

    private suspend fun reconcileAfterDelete(
        configuration: AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
        generation: Long,
        previous: DeviceSessionsState,
        target: DeviceSessionSummary,
        deleteConfirmed: Boolean,
    ): Boolean {
        return try {
            val listed = mapResponse(transport.listSessions(configuration), binding)
            if (listed.sessions.none { it.id == target.id }) {
                publish(
                    binding,
                    generation,
                    listed.copy(message = "${target.deviceLabel} was revoked."),
                    advancePresentation = true,
                )
                true
            } else {
                publish(
                    binding,
                    generation,
                    listed.copy(
                        phase = DeviceSessionsPhase.STALE,
                        message = "The server still reports ${target.deviceLabel} as active.",
                    ),
                    advancePresentation = true,
                )
                false
            }
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            val withoutConfirmedTarget = if (deleteConfirmed) {
                previous.sessions.filterNot { it.id == target.id }
            } else {
                previous.sessions
            }
            val fallback = failureState(
                error,
                previous.copy(sessions = withoutConfirmedTarget),
            ).copy(
                isBusy = false,
                revokingSessionId = null,
                message = if (deleteConfirmed) {
                    "${target.deviceLabel} was revoked · the remaining list could not be refreshed"
                } else {
                    "Revocation outcome is unconfirmed · refresh active devices"
                },
            )
            publish(binding, generation, fallback)
            deleteConfirmed
        }
    }

    private fun mapResponse(
        response: DeviceSessionListResponse,
        binding: ApiConnectionSnapshot,
    ): DeviceSessionsState {
        require(
            binding.hasBearerToken && binding.configurationId != null &&
                binding.clientInstanceId != null,
        )
        require(response.sessions.size <= MAX_ACTIVE_DEVICE_SESSIONS)
        require(response.sessions.map { it.id }.toSet().size == response.sessions.size)
        val receivedAt = now()
        response.sessions.forEach { validateListedDeviceSession(it, receivedAt) }
        validateListedDeviceSessionOrder(response.sessions)
        val currentSession = response.sessions.singleOrNull { it.id == binding.configurationId }
        require(currentSession != null) {
            "The authenticated active-session list omitted its current session"
        }
        require(
            currentSession.clientKind == "android" &&
                currentSession.clientInstanceId == binding.clientInstanceId,
        ) { "The current active-session row did not match this Android device" }
        val currentSessionCanRevoke = "auth_sessions_write" in currentSession.scopes
        val summaries = response.sessions.map { session ->
            session.toSummary(isCurrent = session.id == binding.configurationId)
        }.sortedWith(
            compareByDescending<DeviceSessionSummary> { it.isCurrent }
                .thenByDescending { it.lastSeenAt }
                .thenBy { it.id },
        )
        return DeviceSessionsState(
            phase = DeviceSessionsPhase.READY,
            sessions = summaries,
            lastRefreshedAt = receivedAt,
            message = if (currentSessionCanRevoke) {
                "${summaries.size} active ${if (summaries.size == 1) "device" else "devices"}"
            } else {
                "${summaries.size} active ${if (summaries.size == 1) "device" else "devices"} · Read-only access"
            },
            configurationId = binding.configurationId,
            clientInstanceId = binding.clientInstanceId,
            currentSessionCanRevoke = currentSessionCanRevoke,
        )
    }

    private fun DeviceSessionContract.toSummary(isCurrent: Boolean) = DeviceSessionSummary(
        id = id,
        clientKind = clientKind,
        deviceLabel = deviceLabel,
        clientVersion = clientVersion,
        createdAt = Instant.parse(createdAt),
        lastSeenAt = Instant.parse(lastSeenAt),
        refreshIdleExpiresAt = Instant.parse(refreshIdleExpiresAt),
        absoluteExpiresAt = Instant.parse(absoluteExpiresAt),
        revision = revision,
        isCurrent = isCurrent,
    )

    private fun resolveConfiguration(
        binding: ApiConnectionSnapshot,
        generation: Long,
    ): AuthenticatedApiConfiguration? {
        if (
            !binding.hasBearerToken || binding.baseUrl == null || binding.configurationId == null ||
            binding.clientInstanceId == null
        ) {
            publishRaw(generation, initialState(binding))
            return null
        }
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            null
        }
        if (
            configuration == null || configuration.configurationId != binding.configurationId ||
            configuration.baseUrl.toString() != binding.baseUrl
        ) {
            publishRaw(
                generation,
                DeviceSessionsState(
                    phase = DeviceSessionsPhase.AUTH_REQUIRED,
                    message = "Reconnect this device to manage active sessions.",
                    configurationId = binding.configurationId,
                    clientInstanceId = binding.clientInstanceId,
                ),
            )
            return null
        }
        return configuration
    }

    private fun failureState(
        error: Exception,
        previous: DeviceSessionsState,
    ): DeviceSessionsState {
        if (error is ApiBindingChangedException || error is DeviceAuthenticationChangedException) {
            invalidateForCurrentBinding()
            return initialState(visibleBinding())
        }
        val hasRows = previous.sessions.isNotEmpty()
        val phase: DeviceSessionsPhase
        val message: String
        when (error) {
            is DeviceAuthenticationRequiredException,
            is DeviceAuthApiException.Authentication,
            -> {
                phase = DeviceSessionsPhase.AUTH_REQUIRED
                message = "This device must be re-enrolled before sessions can be managed."
            }
            is DeviceAuthApiException.Forbidden -> {
                phase = if (hasRows) DeviceSessionsPhase.STALE else DeviceSessionsPhase.ERROR
                message = "This device does not have session-management permission."
            }
            is DeviceAuthApiException.InvalidResponse,
            is IllegalArgumentException,
            -> {
                phase = if (hasRows) DeviceSessionsPhase.STALE else DeviceSessionsPhase.ERROR
                message = "The active-device response was invalid · no new state was accepted."
            }
            is DeviceAuthApiException.Unavailable -> {
                phase = if (hasRows) DeviceSessionsPhase.STALE else DeviceSessionsPhase.ERROR
                message = "Active devices could not be verified · refresh to try again."
            }
            is IOException -> {
                phase = DeviceSessionsPhase.OFFLINE
                message = if (hasRows) {
                    "Offline · this in-memory list may be outdated."
                } else {
                    "Offline · connect to load active devices."
                }
            }
            else -> {
                phase = if (hasRows) DeviceSessionsPhase.STALE else DeviceSessionsPhase.ERROR
                message = "Active devices could not be updated."
            }
        }
        val clearRows = phase == DeviceSessionsPhase.AUTH_REQUIRED
        return previous.copy(
            phase = phase,
            sessions = if (clearRows) emptyList() else previous.sessions,
            isBusy = false,
            revokingSessionId = null,
            message = message,
        )
    }

    private fun Exception.isAmbiguousOrMissingDelete(): Boolean = when (this) {
        is ApiBindingChangedException,
        is DeviceAuthenticationChangedException,
        is DeviceAuthenticationRequiredException,
        -> false
        is DeviceSessionDeleteOutcomeAmbiguousException -> true
        is DeviceAuthApiException.Unavailable -> true
        is DeviceAuthApiException.Http ->
            statusCode == 404 || statusCode in setOf(408, 425, 429) || statusCode in 500..599
        is DeviceAuthApiException -> false
        is IOException -> true
        else -> false
    }

    private fun stateForBinding(
        binding: ApiConnectionSnapshot,
        generation: Long,
    ): DeviceSessionsState = synchronized(presentationFence) {
        mutableState.value.takeIf {
            operationAllowed() && presentationGeneration == generation &&
                it.configurationId == binding.configurationId &&
                it.clientInstanceId == binding.clientInstanceId
        } ?: initialState(binding)
    }

    private fun publish(
        binding: ApiConnectionSnapshot,
        generation: Long,
        next: DeviceSessionsState,
        advancePresentation: Boolean = false,
    ): Boolean {
        val latest = credentialStore.snapshot()
        return synchronized(presentationFence) {
            if (
                !operationAllowed() || presentationGeneration != generation ||
                latest.baseUrl != binding.baseUrl || latest.configurationId != binding.configurationId ||
                latest.hasBearerToken != binding.hasBearerToken ||
                latest.clientInstanceId != binding.clientInstanceId
            ) {
                if (presentationGeneration == generation) {
                    presentationGeneration = nextGeneration(presentationGeneration)
                    mutableState.value = initialState(latest.takeIf { operationAllowed() }
                        ?: QUARANTINED_BINDING)
                }
                return@synchronized false
            }
            mutableState.value = next
            if (advancePresentation) {
                presentationGeneration = nextGeneration(presentationGeneration)
            }
            true
        }
    }

    private fun publishRaw(generation: Long, next: DeviceSessionsState): Boolean =
        synchronized(presentationFence) {
            if (!operationAllowed() || presentationGeneration != generation) return@synchronized false
            mutableState.value = next
            true
        }

    private fun operationCurrent(binding: ApiConnectionSnapshot, generation: Long): Boolean =
        presentationCurrent(generation) && credentialStore.snapshot().let {
            it.baseUrl == binding.baseUrl && it.hasBearerToken == binding.hasBearerToken &&
                it.configurationId == binding.configurationId &&
                it.clientInstanceId == binding.clientInstanceId
        }

    private fun presentationCurrent(generation: Long): Boolean = synchronized(presentationFence) {
        operationAllowed() && presentationGeneration == generation
    }

    private fun invalidateForCurrentBinding() = synchronized(presentationFence) {
        presentationGeneration = nextGeneration(presentationGeneration)
        mutableState.value = initialState(visibleBinding())
    }

    private fun visibleBinding(): ApiConnectionSnapshot =
        if (operationAllowed()) credentialStore.snapshot() else QUARANTINED_BINDING

    private suspend fun <T> withPresentationOperation(
        expectedGeneration: Long? = null,
        block: suspend (Long) -> T,
    ): T? {
        val job = currentCoroutineContext()[Job] ?: return null
        val generation = synchronized(presentationFence) {
            if (
                !operationAllowed() ||
                expectedGeneration != null && expectedGeneration != presentationGeneration
            ) {
                return@synchronized null
            }
            activeOperationJobs[job] = Math.incrementExact(activeOperationJobs[job] ?: 0)
            presentationGeneration
        } ?: return null
        return try {
            block(generation)
        } finally {
            synchronized(presentationFence) {
                val count = activeOperationJobs[job] ?: 0
                if (count <= 1) activeOperationJobs.remove(job) else activeOperationJobs[job] = count - 1
            }
        }
    }

    private fun initialState(binding: ApiConnectionSnapshot): DeviceSessionsState =
        if (
            binding.hasBearerToken && binding.configurationId != null &&
            binding.clientInstanceId != null
        ) {
            DeviceSessionsState(
                phase = DeviceSessionsPhase.STALE,
                message = "Refresh to load active devices.",
                configurationId = binding.configurationId,
                clientInstanceId = binding.clientInstanceId,
            )
        } else if (binding.hasBearerToken) {
            DeviceSessionsState(
                phase = DeviceSessionsPhase.AUTH_REQUIRED,
                message = "Reconnect this device to verify its session identity.",
                configurationId = binding.configurationId,
                clientInstanceId = binding.clientInstanceId,
            )
        } else {
            DeviceSessionsState(
                phase = DeviceSessionsPhase.NOT_CONFIGURED,
                message = "Connect this device to manage active sessions.",
            )
        }

    private fun nextGeneration(current: Long): Long =
        if (current == Long.MAX_VALUE) 1L else current + 1L

    private companion object {
        val QUARANTINED_BINDING = ApiConnectionSnapshot(null, false, null, null)
    }
}
