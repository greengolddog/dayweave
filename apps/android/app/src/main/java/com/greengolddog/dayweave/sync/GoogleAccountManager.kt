package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.GoogleAccountsApiException
import com.greengolddog.dayweave.network.GoogleAccountsTransport
import com.greengolddog.dayweave.network.RemoteGoogleAccount
import com.greengolddog.dayweave.network.RemoteGoogleAccounts
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import java.io.IOException
import java.net.URI
import java.time.Instant
import java.util.UUID
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
    val hasCalendar: Boolean,
    val hasTasks: Boolean,
    val revision: Long,
)

data class PendingGoogleAuthorization(
    val url: String,
    val expiresAt: Instant,
) {
    override fun toString(): String =
        "PendingGoogleAuthorization(url=<redacted>, expiresAt=$expiresAt)"
}

data class GoogleAccountState(
    val phase: GoogleAccountPhase,
    val accounts: List<GoogleAccountSummary> = emptyList(),
    val authorization: PendingGoogleAuthorization? = null,
    val message: String,
    val isBusy: Boolean = false,
    val requiresPlannerApiConfiguration: Boolean = false,
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

    suspend fun refresh() = operationMutex.withLock {
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            mutableState.value = GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Planner credentials are unavailable · reconnect the DayWeave API",
                requiresPlannerApiConfiguration = true,
            )
            return@withLock
        }
        if (configuration == null) {
            mutableState.value = initialState(binding)
            return@withLock
        }
        val previous = mutableState.value
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
            )
        } catch (error: Exception) {
            if (!bindingStillCurrent(binding)) return@withLock
            mutableState.value = failureState(error, previous.copy(isBusy = false))
        }
    }

    suspend fun connectNew() = beginAuthorization(account = null)

    suspend fun reauthorize(accountId: String) {
        val account = mutableState.value.accounts.firstOrNull { it.id == accountId } ?: return
        beginAuthorization(account)
    }

    suspend fun setPaused(accountId: String, paused: Boolean) = operationMutex.withLock {
        val account = mutableState.value.accounts.firstOrNull { it.id == accountId } ?: return@withLock
        mutateAccount("Updating Google sync…") { configuration ->
            transport.setPaused(
                configuration = configuration,
                accountId = account.id,
                expectedRevision = account.revision,
                paused = paused,
                idempotencyKey = newUuid().toString(),
            )
        }
    }

    suspend fun disconnect(accountId: String) = operationMutex.withLock {
        val account = mutableState.value.accounts.firstOrNull { it.id == accountId } ?: return@withLock
        mutateAccount("Revoking Google access…") { configuration ->
            transport.disconnect(
                configuration = configuration,
                accountId = account.id,
                expectedRevision = account.revision,
                idempotencyKey = newUuid().toString(),
            )
        }
    }

    fun browserOpenFailed() {
        val current = mutableState.value
        if (current.authorization != null) {
            mutableState.value = current.copy(
                phase = GoogleAccountPhase.ERROR,
                message = "Google could not be opened · try the authorization button again",
            )
        }
    }

    private suspend fun beginAuthorization(account: GoogleAccountSummary?) =
        operationMutex.withLock {
            val binding = credentialStore.snapshot()
            val configuration = try {
                credentialStore.authenticatedConfiguration()
            } catch (_: RuntimeException) {
                mutableState.value = GoogleAccountState(
                    phase = GoogleAccountPhase.AUTH_REQUIRED,
                    message = "Reconnect the DayWeave API before connecting Google",
                    requiresPlannerApiConfiguration = true,
                )
                return@withLock
            }
            if (configuration == null) {
                mutableState.value = initialState(binding)
                return@withLock
            }
            val previous = mutableState.value
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
                        services = setOf("calendar", "tasks"),
                        forceConsent = account?.status == "reauthorization_required",
                        accountId = account?.id,
                        connectNew = account == null,
                        makeDefault = account == null && previous.accounts.none { it.isDefault },
                    ),
                )
                if (!bindingStillCurrent(binding)) return@withLock
                val expiresAt = Instant.parse(started.expiresAt)
                require(expiresAt > now() && expiresAt <= now().plusSeconds(MAX_AUTHORIZATION_SECONDS))
                validateGoogleAuthorizationUrl(started.authorizationUrl)
                mutableState.value = previous.copy(
                    phase = GoogleAccountPhase.AWAITING_BROWSER,
                    authorization = PendingGoogleAuthorization(started.authorizationUrl, expiresAt),
                    isBusy = false,
                    message = "Authorize in Google, return here, then refresh status",
                )
            } catch (error: Exception) {
                if (!bindingStillCurrent(binding)) return@withLock
                mutableState.value = failureState(error, previous.copy(isBusy = false))
            }
        }

    private suspend fun mutateAccount(
        progressMessage: String,
        mutation: suspend (com.greengolddog.dayweave.network.AuthenticatedApiConfiguration) ->
            RemoteGoogleAccount,
    ) {
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            mutableState.value = GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Reconnect the DayWeave API before changing Google access",
                requiresPlannerApiConfiguration = true,
            )
            return
        }
        if (configuration == null) {
            mutableState.value = initialState(binding)
            return
        }
        val previous = mutableState.value
        mutableState.value = previous.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = progressMessage,
        )
        try {
            validateAccount(mutation(configuration))
            if (!bindingStillCurrent(binding)) return
            val refreshed = transport.accounts(configuration)
            if (!bindingStillCurrent(binding)) return
            mutableState.value = mapState(refreshed, authorization = null)
        } catch (error: GoogleAccountsApiException.Conflict) {
            if (!bindingStillCurrent(binding)) return
            try {
                mutableState.value = mapState(transport.accounts(configuration), authorization = null)
            } catch (refreshError: Exception) {
                mutableState.value = failureState(refreshError, previous.copy(isBusy = false))
            }
        } catch (error: Exception) {
            if (!bindingStillCurrent(binding)) return
            mutableState.value = failureState(error, previous.copy(isBusy = false))
        }
    }

    private fun mapState(
        response: RemoteGoogleAccounts,
        authorization: PendingGoogleAuthorization?,
    ): GoogleAccountState {
        validateCleanup(response)
        val accounts = response.accounts.map(::validateAccount)
            .filter { it.status != "revoked" }
            .sortedWith(compareByDescending<GoogleAccountSummary> { it.isDefault }.thenBy { it.label })
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
            authorization != null -> GoogleAccountState(
                phase = GoogleAccountPhase.AWAITING_BROWSER,
                accounts = accounts,
                authorization = authorization,
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
        }
    }

    private fun validateAccount(remote: RemoteGoogleAccount): GoogleAccountSummary {
        require(UUID.fromString(remote.id).toString() == remote.id)
        require(remote.externalAccountId.isSafeLabel() && remote.displayLabel.isSafeLabel())
        require(remote.status in GOOGLE_ACCOUNT_STATUSES && remote.revision > 0)
        val createdAt = Instant.parse(remote.createdAt)
        val updatedAt = Instant.parse(remote.updatedAt)
        require(updatedAt >= createdAt)
        remote.tokenExpiresAt?.let(Instant::parse)
        require(remote.grantedScopes.size <= MAX_SCOPES && remote.grantedScopes.all { scope ->
            scope.length in 1..MAX_SCOPE_CHARS && !scope.any(Char::isISOControl)
        })
        return GoogleAccountSummary(
            id = remote.id,
            label = remote.displayLabel,
            status = remote.status,
            syncEnabled = remote.syncEnabled,
            isDefault = remote.isDefault,
            hasCalendar = GOOGLE_CALENDAR_SCOPE in remote.grantedScopes,
            hasTasks = GOOGLE_TASKS_SCOPE in remote.grantedScopes,
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
        val (phase, message) = when (error) {
            is GoogleAccountsApiException.Authentication ->
                GoogleAccountPhase.AUTH_REQUIRED to "Planner API authentication is required"
            is GoogleAccountsApiException.Unavailable ->
                GoogleAccountPhase.ERROR to "Google authorization is not configured on the server"
            is GoogleAccountsApiException.Validation ->
                GoogleAccountPhase.ERROR to "Google rejected this connection request"
            is GoogleAccountsApiException.InvalidResponse, is IllegalArgumentException ->
                GoogleAccountPhase.ERROR to "Google connection response was invalid · no state changed"
            is IOException ->
                GoogleAccountPhase.OFFLINE to "Offline · Google connection state may be outdated"
            else -> GoogleAccountPhase.ERROR to "Google connection could not be updated"
        }
        val trustedAuthorization = if (
            error is GoogleAccountsApiException.InvalidResponse || error is IllegalArgumentException
        ) {
            null
        } else {
            previous.authorization
        }
        return previous.copy(
            phase = phase,
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

    companion object {
        private const val MAX_AUTHORIZATION_SECONDS = 15 * 60L
        private const val MAX_AUTHORIZATION_URL_CHARS = 8 * 1024
        private const val MAX_LABEL_CHARS = 320
        private const val MAX_SCOPES = 32
        private const val MAX_SCOPE_CHARS = 512
        private const val GOOGLE_CALENDAR_SCOPE = "https://www.googleapis.com/auth/calendar"
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
                )
            } else {
                GoogleAccountState(
                    phase = GoogleAccountPhase.DISCONNECTED,
                    message = "Google connection has not been checked",
                )
            }
    }
}
