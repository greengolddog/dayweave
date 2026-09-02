package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.GoogleAccountsApiException
import com.greengolddog.dayweave.network.GoogleAccountsTransport
import com.greengolddog.dayweave.network.RemoteGoogleAccount
import com.greengolddog.dayweave.network.RemoteGoogleAccounts
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import java.io.IOException
import java.net.URI
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class GoogleAccountPhase {
    NOT_CONFIGURED,
    LOADING,
    DISCONNECTED,
    CONNECTED,
    AWAITING_BROWSER,
    AUTH_REQUIRED,
    RECOVERY_REQUIRED,
    OFFLINE,
    ERROR,
}

data class GoogleAccountSummary(
    val id: String,
    val label: String,
    val status: String,
    val syncEnabled: Boolean,
    val isDefault: Boolean,
    /** Calendar can be read with either the narrow or full provider scope. */
    val hasCalendar: Boolean,
    /** Full Calendar scope required for the separately approved publishing flow. */
    val hasCalendarWriteScope: Boolean,
    /** Tasks can be imported with either the narrow or full provider scope. */
    val hasTasks: Boolean,
    /** Full Tasks scope reserved for a later explicit sync upgrade. */
    val hasTasksWriteScope: Boolean,
    val revision: Long,
)

data class PendingGoogleAuthorization(
    val url: String,
    val expiresAt: Instant,
    /** Existing account being repaired; null means a new Google account is being connected. */
    val accountId: String? = null,
    /** Accounts present before a connect-new ceremony, used to prove callback completion. */
    val baselineAccountIds: Set<String> = emptySet(),
) {
    override fun toString(): String =
        "PendingGoogleAuthorization(url=<redacted>, expiresAt=$expiresAt, " +
            "target=${if (accountId == null) "new-account" else "existing-account"})"
}

data class GoogleAccountState(
    val phase: GoogleAccountPhase,
    val accounts: List<GoogleAccountSummary> = emptyList(),
    val authorization: PendingGoogleAuthorization? = null,
    val message: String,
    val isBusy: Boolean = false,
    val requiresPlannerApiConfiguration: Boolean = false,
    /** Opaque API credential generation that owns every account and URL in this state. */
    val configurationId: String? = null,
)

class GoogleAccountManager(
    private val credentialStore: ApiCredentialStore,
    private val transport: GoogleAccountsTransport,
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
) {
    private val operationMutex = Mutex()
    private val mutableState = MutableStateFlow(initialState(credentialStore.snapshot()))
    val state: StateFlow<GoogleAccountState> = mutableState.asStateFlow()

    /** Drops account labels and pending browser authority under the binding writer. */
    internal fun quarantineBindingState() {
        mutableState.value = initialState(ApiConnectionSnapshot(null, false, null, null))
    }

    suspend fun refresh() = operationMutex.withLock {
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            mutableState.value = GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Planner credentials are unavailable · reconnect the DayWeave API",
                requiresPlannerApiConfiguration = true,
                configurationId = binding.configurationId,
            )
            return@withLock
        }
        if (configuration == null) {
            mutableState.value = initialState(binding)
            return@withLock
        }
        if (!configurationMatchesBinding(configuration, binding)) return@withLock
        val bindingTicket = try {
            configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            mutableState.value = initialState(credentialStore.snapshot())
            return@withLock
        }
        try {
        val previous = stateForBinding(binding)
        mutableState.value = previous.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = "Checking Google connection…",
        )
        try {
            val response = transport.accounts(configuration)
            if (!bindingStillCurrent(binding)) return@withLock
            mutableState.value = mapState(
                response,
                previous.authorization?.takeIf { it.expiresAt > now() },
                binding.configurationId,
            )
        } catch (error: CancellationException) {
            if (bindingStillCurrent(binding)) mutableState.value = previous.copy(isBusy = false)
            throw error
        } catch (error: Exception) {
            if (!bindingStillCurrent(binding)) return@withLock
            mutableState.value = failureState(error, previous.copy(isBusy = false))
        }
        } finally {
            bindingTicket.release()
        }
    }

    suspend fun connectNew() = beginAuthorization(accountId = null)

    suspend fun reauthorize(accountId: String) = beginAuthorization(accountId)

    suspend fun restartAuthorization() {
        val restart = operationMutex.withLock {
            val current = mutableState.value
            val pending = current.authorization ?: return@withLock null
            RestartAuthorization(pending.accountId, current.configurationId)
        } ?: return
        beginAuthorization(
            accountId = restart.accountId,
            expectedConfigurationId = restart.configurationId,
        )
    }

    suspend fun setPaused(accountId: String, paused: Boolean) =
        mutateAccount(accountId, "Updating Google sync…") { configuration, account ->
            transport.setPaused(
                configuration = configuration,
                accountId = account.id,
                expectedRevision = account.revision,
                paused = paused,
                idempotencyKey = newUuid().toString(),
            )
        }

    suspend fun disconnect(accountId: String) =
        mutateAccount(
            accountId = accountId,
            progressMessage = "Revoking Google access…",
            reconcileUnavailable = true,
        ) { configuration, account ->
            transport.disconnect(
                configuration = configuration,
                accountId = account.id,
                expectedRevision = account.revision,
                idempotencyKey = newUuid().toString(),
            )
        }

    /**
     * Consumes the URL synchronously while holding the same lock used for credential replacement.
     * This closes the otherwise unavoidable check-to-browser-open generation race.
     */
    suspend fun useAuthorizationUrlIfCurrent(
        candidate: String,
        consumer: (String) -> Unit,
    ): Boolean = operationMutex.withLock {
        val current = mutableState.value
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            null
        }
        val bindingTicket = try {
            configuration?.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            null
        }
        val currentBinding = credentialStore.snapshot()
        try {
            val authorization = current.authorization
            val trusted =
                bindingTicket != null && sameBinding(binding, currentBinding) &&
                    binding.hasBearerToken && configuration != null &&
                    configurationMatchesBindingValue(configuration, binding) &&
                    current.configurationId == binding.configurationId &&
                    authorization?.url == candidate && authorization.expiresAt > now()
            if (!trusted) {
                mutableState.value = initialState(currentBinding)
                return@withLock false
            }
            consumer(candidate)
            true
        } finally {
            bindingTicket?.release()
        }
    }

    /** Serializes the only credential update/clear path with all Google requests and URL use. */
    suspend fun <T> withConfigurationChangeLock(change: suspend () -> T): T =
        operationMutex.withLock {
            val before = credentialStore.snapshot()
            try {
                change()
            } finally {
                val after = credentialStore.snapshot()
                if (!sameBinding(before, after)) {
                    mutableState.value = initialState(after)
                }
            }
        }

    suspend fun browserOpenFailed() = operationMutex.withLock {
        val current = mutableState.value
        val binding = credentialStore.snapshot()
        if (!binding.hasBearerToken || current.configurationId != binding.configurationId) {
            mutableState.value = initialState(binding)
        } else if (current.authorization != null) {
            mutableState.value = current.copy(
                phase = GoogleAccountPhase.ERROR,
                message = "Google could not be opened · try the authorization button again",
            )
        }
    }

    private suspend fun beginAuthorization(
        accountId: String?,
        expectedConfigurationId: String? = null,
    ) =
        operationMutex.withLock {
            val binding = credentialStore.snapshot()
            if (
                expectedConfigurationId != null &&
                expectedConfigurationId != binding.configurationId
            ) {
                mutableState.value = initialState(binding)
                return@withLock
            }
            val configuration = try {
                credentialStore.authenticatedConfiguration()
            } catch (_: RuntimeException) {
                mutableState.value = GoogleAccountState(
                    phase = GoogleAccountPhase.AUTH_REQUIRED,
                    message = "Reconnect the DayWeave API before connecting Google",
                    requiresPlannerApiConfiguration = true,
                    configurationId = binding.configurationId,
                )
                return@withLock
            }
            if (configuration == null) {
                mutableState.value = initialState(binding)
                return@withLock
            }
            if (!configurationMatchesBinding(configuration, binding)) return@withLock
            val bindingTicket = try {
                configuration.beginBindingOperation()
            } catch (_: ApiBindingChangedException) {
                mutableState.value = initialState(credentialStore.snapshot())
                return@withLock
            }
            try {
            val previous = stateForBinding(binding)
            val account = accountId?.let { requestedId ->
                previous.accounts.firstOrNull { it.id == requestedId }
            }
            if (accountId != null && account == null) {
                mutableState.value = operationFailureStatePreservingRecovery(
                    previous,
                    "That Google account belongs to an older API connection · refresh status",
                )
                return@withLock
            }
            mutableState.value = previous.copy(
                phase = GoogleAccountPhase.LOADING,
                isBusy = true,
                message = "Preparing private Google authorization…",
            )
            try {
                val started = transport.startAuthorization(
                    configuration = configuration,
                    idempotencyKey = newUuid().toString(),
                    request = StartGoogleAuthorizationRequest(
                        // The explicit empty sentinel asks the server for Calendar and Tasks
                        // read-only. Full scopes remain separate existing-account upgrades.
                        services = emptyList(),
                        forceConsent = account != null,
                        accountId = account?.id,
                        connectNew = account == null && previous.accounts.isNotEmpty(),
                        makeDefault = account?.isDefault
                            ?: previous.accounts.none { it.isDefault },
                    ),
                )
                if (!bindingStillCurrent(binding)) return@withLock
                val expiresAt = Instant.parse(started.expiresAt)
                require(expiresAt > now() && expiresAt <= now().plusSeconds(MAX_AUTHORIZATION_SECONDS))
                validateGoogleAuthorizationUrl(started.authorizationUrl)
                mutableState.value = previous.copy(
                    phase = GoogleAccountPhase.AWAITING_BROWSER,
                    authorization = PendingGoogleAuthorization(
                        url = started.authorizationUrl,
                        expiresAt = expiresAt,
                        accountId = account?.id,
                        baselineAccountIds = previous.accounts.mapTo(mutableSetOf()) { it.id },
                    ),
                    isBusy = false,
                    message = "Authorize in Google, return here, then refresh status",
                )
            } catch (error: CancellationException) {
                if (bindingStillCurrent(binding)) mutableState.value = previous.copy(isBusy = false)
                throw error
            } catch (error: Exception) {
                if (!bindingStillCurrent(binding)) return@withLock
                mutableState.value = failureState(error, previous.copy(isBusy = false))
            }
            } finally {
                bindingTicket.release()
            }
        }

    private suspend fun mutateAccount(
        accountId: String,
        progressMessage: String,
        reconcileUnavailable: Boolean = false,
        mutation: suspend (
            com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
            GoogleAccountSummary,
        ) -> RemoteGoogleAccount,
    ) = operationMutex.withLock {
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            mutableState.value = GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Reconnect the DayWeave API before changing Google access",
                requiresPlannerApiConfiguration = true,
                configurationId = binding.configurationId,
            )
            return@withLock
        }
        if (configuration == null) {
            mutableState.value = initialState(binding)
            return@withLock
        }
        if (!configurationMatchesBinding(configuration, binding)) return@withLock
        val bindingTicket = try {
            configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            mutableState.value = initialState(credentialStore.snapshot())
            return@withLock
        }
        try {
        val previous = stateForBinding(binding)
        val account = previous.accounts.firstOrNull { it.id == accountId }
        if (account == null) {
            mutableState.value = operationFailureStatePreservingRecovery(
                previous,
                "That Google account belongs to an older API connection · refresh status",
            )
            return@withLock
        }
        mutableState.value = previous.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = progressMessage,
        )
        try {
            validateAccount(mutation(configuration, account))
            if (!bindingStillCurrent(binding)) return@withLock
            val refreshed = transport.accounts(configuration)
            if (!bindingStillCurrent(binding)) return@withLock
            mutableState.value = mapState(
                refreshed,
                authorization = null,
                configurationId = binding.configurationId,
            )
        } catch (error: GoogleAccountsApiException.Unavailable) {
            if (!bindingStillCurrent(binding)) return@withLock
            if (!reconcileUnavailable) {
                mutableState.value = failureState(error, previous.copy(isBusy = false))
                return@withLock
            }
            reconcileAmbiguousDisconnect(
                configuration = configuration,
                binding = binding,
                accountId = accountId,
                previous = previous,
                unavailable = true,
            )
        } catch (error: GoogleAccountsApiException.Conflict) {
            if (!bindingStillCurrent(binding)) return@withLock
            try {
                val refreshed = transport.accounts(configuration)
                if (!bindingStillCurrent(binding)) return@withLock
                mutableState.value = mapState(
                    refreshed,
                    authorization = null,
                    configurationId = binding.configurationId,
                )
            } catch (refreshError: CancellationException) {
                if (bindingStillCurrent(binding)) {
                    mutableState.value = operationFailureStatePreservingRecovery(
                        previous,
                        "Google account reconciliation was interrupted · refresh status",
                    )
                }
                throw refreshError
            } catch (refreshError: Exception) {
                if (!bindingStillCurrent(binding)) return@withLock
                mutableState.value = failureState(refreshError, previous.copy(isBusy = false))
            }
        } catch (error: GoogleAccountsApiException.Http) {
            if (!bindingStillCurrent(binding)) return@withLock
            if (reconcileUnavailable && error.statusCode == 404) {
                reconcileAmbiguousDisconnect(
                    configuration = configuration,
                    binding = binding,
                    accountId = accountId,
                    previous = previous,
                    unavailable = false,
                )
            } else {
                mutableState.value = failureState(error, previous.copy(isBusy = false))
            }
        } catch (error: CancellationException) {
            if (bindingStillCurrent(binding)) {
                mutableState.value = operationFailureStatePreservingRecovery(
                    previous,
                    "Google update outcome is unknown · refresh status before retrying",
                )
            }
            throw error
        } catch (error: Exception) {
            if (!bindingStillCurrent(binding)) return@withLock
            mutableState.value = failureState(error, previous.copy(isBusy = false))
        }
        } finally {
            bindingTicket.release()
        }
    }

    private suspend fun reconcileAmbiguousDisconnect(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
        accountId: String,
        previous: GoogleAccountState,
        unavailable: Boolean,
    ) {
        try {
            val refreshed = transport.accounts(configuration)
            if (!bindingStillCurrent(binding)) return
            val mapped = mapState(
                refreshed,
                authorization = null,
                configurationId = binding.configurationId,
            )
            mutableState.value = if (mapped.phase == GoogleAccountPhase.RECOVERY_REQUIRED) {
                mapped
            } else if (mapped.accounts.any { it.id == accountId }) {
                mapped.copy(
                    phase = GoogleAccountPhase.ERROR,
                    message = if (unavailable) {
                        "Google access was not confirmed revoked · encrypted credentials remain for a safe retry"
                    } else {
                        "Google disconnect changed on the server · review status and retry if needed"
                    },
                )
            } else {
                mapped
            }
        } catch (error: CancellationException) {
            if (bindingStillCurrent(binding)) {
                mutableState.value = operationFailureStatePreservingRecovery(
                    previous,
                    "Google disconnect outcome is unknown · refresh status before retrying",
                )
            }
            throw error
        } catch (_: Exception) {
            if (!bindingStillCurrent(binding)) return
            mutableState.value = operationFailureStatePreservingRecovery(
                previous,
                "Google access was not confirmed revoked · refresh status before retrying",
            )
        }
    }

    private fun operationFailureStatePreservingRecovery(
        previous: GoogleAccountState,
        message: String,
    ): GoogleAccountState = if (previous.phase == GoogleAccountPhase.RECOVERY_REQUIRED) {
        previous.copy(authorization = null, isBusy = false)
    } else {
        previous.copy(
            phase = GoogleAccountPhase.ERROR,
            authorization = null,
            isBusy = false,
            message = message,
        )
    }

    private fun mapState(
        response: RemoteGoogleAccounts,
        authorization: PendingGoogleAuthorization?,
        configurationId: String?,
    ): GoogleAccountState {
        validateCleanup(response)
        require(response.accounts.size <= MAX_ACCOUNTS)
        require(response.accounts.map { it.id }.toSet().size == response.accounts.size)
        require(
            response.accounts.map { it.externalAccountId }.toSet().size == response.accounts.size,
        )
        require(response.accounts.count { it.isDefault } <= 1)
        val accounts = response.accounts.map(::validateAccount)
            .filter { it.status != "revoked" }
            .sortedWith(compareByDescending<GoogleAccountSummary> { it.isDefault }.thenBy { it.label })
        val unresolvedAuthorization = authorization?.takeUnless { pending ->
            if (pending.accountId != null) {
                accounts.any { account ->
                    account.id == pending.accountId && account.status in setOf("active", "paused")
                }
            } else {
                accounts.any { it.id !in pending.baselineAccountIds }
            }
        }
        val operatorRecovery = response.cleanup.operatorRecoveryRequired ||
            response.cleanup.legacyRecoveryRequired > 0
        val recovery = operatorRecovery || response.cleanup.durabilityDegraded ||
            response.cleanup.revocationFenced || response.cleanup.exhausted > 0 ||
            response.cleanup.uncertainAuthorizations > 0
        val needsAuthorization = accounts.any { it.status == "reauthorization_required" }
        val revocationFailed = accounts.any { it.status == "revocation_failed" }
        return when {
            recovery -> GoogleAccountState(
                phase = GoogleAccountPhase.RECOVERY_REQUIRED,
                accounts = accounts,
                message = if (operatorRecovery) {
                    "Google credential recovery needs owner attention on the server"
                } else {
                    "Google credential cleanup is fenced · the server will retry safely"
                },
            )
            unresolvedAuthorization != null -> GoogleAccountState(
                phase = GoogleAccountPhase.AWAITING_BROWSER,
                accounts = accounts,
                authorization = unresolvedAuthorization,
                message = "Finish authorization in Google, then refresh status",
            )
            revocationFailed -> GoogleAccountState(
                phase = GoogleAccountPhase.ERROR,
                accounts = accounts,
                message = "Google revocation failed · retry Disconnect when the network recovers",
            )
            needsAuthorization -> GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                accounts = accounts,
                message = "One or more Google accounts need authorization",
            )
            accounts.any { it.status == "active" } -> GoogleAccountState(
                phase = GoogleAccountPhase.CONNECTED,
                accounts = accounts,
                message = "Google Calendar and Tasks connected",
            )
            accounts.isNotEmpty() -> GoogleAccountState(
                phase = GoogleAccountPhase.CONNECTED,
                accounts = accounts,
                message = "Google connection paused",
            )
            else -> GoogleAccountState(
                phase = GoogleAccountPhase.DISCONNECTED,
                message = "Google Calendar and Tasks are not connected",
            )
        }.copy(configurationId = configurationId)
    }

    private fun validateAccount(remote: RemoteGoogleAccount): GoogleAccountSummary {
        require(UUID.fromString(remote.id).toString() == remote.id)
        require(remote.externalAccountId.isSafeLabel() && remote.displayLabel.isSafeLabel())
        require(remote.status in GOOGLE_ACCOUNT_STATUSES && remote.revision > 0)
        val createdAt = Instant.parse(remote.createdAt)
        val updatedAt = Instant.parse(remote.updatedAt)
        require(updatedAt >= createdAt)
        remote.tokenExpiresAt?.let(Instant::parse)
        require(
            remote.grantedScopes.size <= MAX_SCOPES &&
                remote.grantedScopes.toSet().size == remote.grantedScopes.size &&
                remote.grantedScopes.all { scope ->
                    scope.length in 1..MAX_SCOPE_CHARS && !scope.any(Char::isISOControl)
                },
        )
        require(remote.syncEnabled == (remote.status == "active"))
        require(
            if (remote.status == "revoked") {
                remote.grantedScopes.isEmpty() && remote.tokenExpiresAt == null && !remote.isDefault
            } else {
                remote.grantedScopes.isNotEmpty()
            },
        )
        val hasCalendarWriteScope = GOOGLE_CALENDAR_SCOPE in remote.grantedScopes
        val hasTasksWriteScope = GOOGLE_TASKS_SCOPE in remote.grantedScopes
        return GoogleAccountSummary(
            id = remote.id,
            label = remote.displayLabel,
            status = remote.status,
            syncEnabled = remote.syncEnabled,
            isDefault = remote.isDefault,
            hasCalendar = hasCalendarWriteScope ||
                GOOGLE_CALENDAR_READ_ONLY_SCOPE in remote.grantedScopes,
            hasCalendarWriteScope = hasCalendarWriteScope,
            hasTasks = hasTasksWriteScope || GOOGLE_TASKS_READ_ONLY_SCOPE in remote.grantedScopes,
            hasTasksWriteScope = hasTasksWriteScope,
            revision = remote.revision,
        )
    }

    private fun validateCleanup(response: RemoteGoogleAccounts) {
        val cleanup = response.cleanup
        require(
            listOf(
                cleanup.held,
                cleanup.pending,
                cleanup.retrying,
                cleanup.exhausted,
                cleanup.volatileGuardians,
                cleanup.uncertainAuthorizations,
                cleanup.legacyRecoveryRequired,
            ).all { it >= 0 },
        )
        cleanup.nextAttemptAt?.let(Instant::parse)
        cleanup.lastFailureAt?.let(Instant::parse)
    }

    private fun failureState(error: Exception, previous: GoogleAccountState): GoogleAccountState {
        if (
            previous.phase == GoogleAccountPhase.RECOVERY_REQUIRED &&
            error !is GoogleAccountsApiException.Authentication
        ) {
            return previous.copy(authorization = null, isBusy = false)
        }
        val (phase, message) = when (error) {
            is GoogleAccountsApiException.Authentication ->
                GoogleAccountPhase.AUTH_REQUIRED to "Planner API authentication is required"
            is GoogleAccountsApiException.Unavailable ->
                GoogleAccountPhase.ERROR to "Google authorization is not configured on the server"
            is GoogleAccountsApiException.Conflict ->
                GoogleAccountPhase.ERROR to "Google connection is already changing · refresh status"
            is GoogleAccountsApiException.Validation ->
                GoogleAccountPhase.ERROR to "Google rejected this connection request"
            is GoogleAccountsApiException.InvalidResponse, is IllegalArgumentException ->
                GoogleAccountPhase.ERROR to "Google connection response was invalid · no state changed"
            is GoogleAccountsApiException.Http ->
                GoogleAccountPhase.ERROR to "The DayWeave server could not update Google access"
            is IOException ->
                GoogleAccountPhase.OFFLINE to "Offline · Google connection state may be outdated"
            else -> GoogleAccountPhase.ERROR to "Google connection could not be updated"
        }
        val clearCachedIdentity = error is GoogleAccountsApiException.Authentication
        val trustedAuthorization = if (
            clearCachedIdentity || error is GoogleAccountsApiException.InvalidResponse ||
            error is IllegalArgumentException
        ) {
            null
        } else {
            previous.authorization
        }
        return previous.copy(
            phase = phase,
            accounts = if (clearCachedIdentity) emptyList() else previous.accounts,
            authorization = trustedAuthorization,
            isBusy = false,
            message = message,
            requiresPlannerApiConfiguration = error is GoogleAccountsApiException.Authentication,
        )
    }

    private fun validateGoogleAuthorizationUrl(value: String) {
        require(value.length in 1..MAX_AUTHORIZATION_URL_CHARS)
        val uri = URI(value)
        require(
            uri.scheme == "https" && uri.host == "accounts.google.com" &&
                (uri.port == -1 || uri.port == 443) && uri.userInfo == null &&
                uri.fragment == null && uri.path == "/o/oauth2/v2/auth" && !uri.rawQuery.isNullOrBlank(),
        ) { "Google authorization URL is untrusted" }
    }

    private fun String.isSafeLabel(): Boolean =
        length in 1..MAX_LABEL_CHARS && !any(Char::isISOControl)

    private fun sameBinding(before: ApiConnectionSnapshot, after: ApiConnectionSnapshot): Boolean =
        before.baseUrl == after.baseUrl && before.configurationId == after.configurationId &&
            before.hasBearerToken == after.hasBearerToken

    private fun bindingStillCurrent(before: ApiConnectionSnapshot): Boolean {
        val current = credentialStore.snapshot()
        if (sameBinding(before, current)) return true
        mutableState.value = initialState(current)
        return false
    }

    private fun configurationMatchesBinding(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
    ): Boolean {
        if (configurationMatchesBindingValue(configuration, binding)) {
            return true
        }
        mutableState.value = initialState(credentialStore.snapshot())
        return false
    }

    private fun configurationMatchesBindingValue(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
    ): Boolean =
        binding.hasBearerToken && binding.configurationId != null &&
            configuration.configurationId == binding.configurationId &&
            configuration.baseUrl.toString() == binding.baseUrl

    private fun stateForBinding(binding: ApiConnectionSnapshot): GoogleAccountState =
        mutableState.value.takeIf { state ->
            binding.hasBearerToken && state.configurationId != null &&
                state.configurationId == binding.configurationId
        } ?: initialState(binding)

    private data class RestartAuthorization(
        val accountId: String?,
        val configurationId: String?,
    )

    companion object {
        private const val MAX_AUTHORIZATION_SECONDS = 15 * 60L
        private const val MAX_AUTHORIZATION_URL_CHARS = 8 * 1024
        private const val MAX_LABEL_CHARS = 320
        private const val MAX_ACCOUNTS = 10_000
        private const val MAX_SCOPES = 32
        private const val MAX_SCOPE_CHARS = 512
        private const val GOOGLE_CALENDAR_READ_ONLY_SCOPE =
            "https://www.googleapis.com/auth/calendar.readonly"
        private const val GOOGLE_CALENDAR_SCOPE = "https://www.googleapis.com/auth/calendar"
        private const val GOOGLE_TASKS_READ_ONLY_SCOPE =
            "https://www.googleapis.com/auth/tasks.readonly"
        private const val GOOGLE_TASKS_SCOPE = "https://www.googleapis.com/auth/tasks"
        private val GOOGLE_ACCOUNT_STATUSES = setOf(
            "active",
            "paused",
            "reauthorization_required",
            "disconnecting",
            "revocation_failed",
            "revoked",
        )

        private fun initialState(snapshot: ApiConnectionSnapshot): GoogleAccountState =
            if (snapshot.baseUrl == null || !snapshot.hasBearerToken) {
                GoogleAccountState(
                    phase = GoogleAccountPhase.NOT_CONFIGURED,
                    message = "Connect the DayWeave API before connecting Google",
                    requiresPlannerApiConfiguration = true,
                    configurationId = snapshot.configurationId,
                )
            } else {
                GoogleAccountState(
                    phase = GoogleAccountPhase.DISCONNECTED,
                    message = "Google connection has not been checked",
                    configurationId = snapshot.configurationId,
                )
            }
    }
}
