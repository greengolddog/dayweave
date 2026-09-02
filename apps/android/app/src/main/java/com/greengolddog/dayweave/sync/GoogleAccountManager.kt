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
    private val operationAllowed: () -> Boolean = { true },
) {
    private val operationMutex = Mutex()
    private val presentationFence = Any()
    private var presentationGeneration = 0L
    private val mutableState = MutableStateFlow(
        initialState(
            credentialStore.snapshot().takeIf { operationAllowed() } ?: QUARANTINED_SNAPSHOT,
        ),
    )
    val state: StateFlow<GoogleAccountState> = mutableState.asStateFlow()

    /** Atomically invalidates in-flight presentation work and drops all private provider data. */
    internal fun quarantineBindingState() {
        invalidatePresentationState(QUARANTINED_SNAPSHOT)
    }

    suspend fun refresh() {
        val presentation = beginPresentationOperation() ?: return
        operationMutex.withLock {
        if (!presentationOperationCurrent(presentation)) return@withLock
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            publishPresentationState(
                presentation,
                GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Planner credentials are unavailable · reconnect the DayWeave API",
                requiresPlannerApiConfiguration = true,
                configurationId = binding.configurationId,
                ),
            )
            return@withLock
        }
        if (configuration == null) {
            publishPresentationState(presentation, initialState(binding))
            return@withLock
        }
        if (!configurationMatchesBinding(configuration, binding, presentation)) return@withLock
        if (!operationStillCurrent(binding, presentation)) return@withLock
        val bindingTicket = try {
            configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            invalidatePresentationState(credentialStore.snapshot())
            return@withLock
        }
        try {
        if (!operationStillCurrent(binding, presentation)) return@withLock
        val previous = stateForBinding(binding, presentation) ?: return@withLock
        if (!publishPresentationState(presentation, previous.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = "Checking Google connection…",
        ))) return@withLock
        try {
            val response = transport.accounts(configuration)
            val mapped = mapState(
                response = response,
                authorization = previous.authorization?.takeIf { it.expiresAt > now() },
                configurationId = binding.configurationId,
            )
            if (!publishOperationState(binding, presentation, mapped)) return@withLock
        } catch (error: CancellationException) {
            publishOperationState(binding, presentation, previous.copy(isBusy = false))
            throw error
        } catch (error: Exception) {
            publishOperationState(
                binding,
                presentation,
                failureState(error, previous.copy(isBusy = false)),
            )
        }
        } finally {
            bindingTicket.release()
        }
        }
    }

    suspend fun connectNew() = beginAuthorization(accountId = null)

    suspend fun reauthorize(accountId: String) = beginAuthorization(accountId)

    suspend fun restartAuthorization() {
        val presentation = beginPresentationOperation() ?: return
        val restart = operationMutex.withLock {
            val current = presentationState(presentation) ?: return@withLock null
            val pending = current.authorization ?: return@withLock null
            RestartAuthorization(pending.accountId, current.configurationId, presentation)
        } ?: return
        beginAuthorization(
            accountId = restart.accountId,
            expectedConfigurationId = restart.configurationId,
            expectedPresentationGeneration = restart.presentationGeneration,
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
    ): Boolean {
        val presentation = beginPresentationOperation() ?: return false
        return operationMutex.withLock {
        val current = presentationState(presentation) ?: return@withLock false
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
            consumeAuthorizationIfCurrent(
                presentation = presentation,
                candidate = candidate,
                binding = binding,
                currentBinding = currentBinding,
                configurationMatches = bindingTicket != null && configuration != null &&
                    configurationMatchesBindingValue(configuration, binding),
                expectedConfigurationId = current.configurationId,
                consumer = consumer,
            ).also { consumed ->
                if (!consumed) {
                    publishPresentationState(presentation, initialState(currentBinding))
                }
            }
        } finally {
            bindingTicket?.release()
        }
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
                    invalidatePresentationState(after)
                }
            }
        }

    suspend fun browserOpenFailed() {
        val presentation = beginPresentationOperation() ?: return
        operationMutex.withLock {
        val current = presentationState(presentation) ?: return@withLock
        val binding = credentialStore.snapshot()
        if (!binding.hasBearerToken || current.configurationId != binding.configurationId) {
            publishPresentationState(presentation, initialState(binding))
        } else if (current.authorization != null) {
            publishPresentationState(
                presentation,
                current.copy(
                    phase = GoogleAccountPhase.ERROR,
                    message = "Google could not be opened · try the authorization button again",
                ),
            )
        }
        }
    }

    private suspend fun beginAuthorization(
        accountId: String?,
        expectedConfigurationId: String? = null,
        expectedPresentationGeneration: Long? = null,
    ) {
        val presentation = beginPresentationOperation(expectedPresentationGeneration) ?: return
        operationMutex.withLock {
            if (!presentationOperationCurrent(presentation)) return@withLock
            val binding = credentialStore.snapshot()
            if (
                expectedConfigurationId != null &&
                expectedConfigurationId != binding.configurationId
            ) {
                publishPresentationState(presentation, initialState(binding))
                return@withLock
            }
            val configuration = try {
                credentialStore.authenticatedConfiguration()
            } catch (_: RuntimeException) {
                publishPresentationState(
                    presentation,
                    GoogleAccountState(
                        phase = GoogleAccountPhase.AUTH_REQUIRED,
                        message = "Reconnect the DayWeave API before connecting Google",
                        requiresPlannerApiConfiguration = true,
                        configurationId = binding.configurationId,
                    ),
                )
                return@withLock
            }
            if (configuration == null) {
                publishPresentationState(presentation, initialState(binding))
                return@withLock
            }
            if (!configurationMatchesBinding(configuration, binding, presentation)) return@withLock
            if (!operationStillCurrent(binding, presentation)) return@withLock
            val bindingTicket = try {
                configuration.beginBindingOperation()
            } catch (_: ApiBindingChangedException) {
                invalidatePresentationState(credentialStore.snapshot())
                return@withLock
            }
            try {
            if (!operationStillCurrent(binding, presentation)) return@withLock
            val previous = stateForBinding(binding, presentation) ?: return@withLock
            val account = accountId?.let { requestedId ->
                previous.accounts.firstOrNull { it.id == requestedId }
            }
            if (accountId != null && account == null) {
                publishPresentationState(
                    presentation,
                    operationFailureStatePreservingRecovery(
                        previous,
                        "That Google account belongs to an older API connection · refresh status",
                    ),
                )
                return@withLock
            }
            if (!publishPresentationState(presentation, previous.copy(
                phase = GoogleAccountPhase.LOADING,
                isBusy = true,
                message = "Preparing private Google authorization…",
            ))) return@withLock
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
                val expiresAt = Instant.parse(started.expiresAt)
                require(expiresAt > now() && expiresAt <= now().plusSeconds(MAX_AUTHORIZATION_SECONDS))
                validateGoogleAuthorizationUrl(started.authorizationUrl)
                publishOperationState(
                    binding,
                    presentation,
                    previous.copy(
                        phase = GoogleAccountPhase.AWAITING_BROWSER,
                        authorization = PendingGoogleAuthorization(
                            url = started.authorizationUrl,
                            expiresAt = expiresAt,
                            accountId = account?.id,
                            baselineAccountIds = previous.accounts.mapTo(mutableSetOf()) { it.id },
                        ),
                        isBusy = false,
                        message = "Authorize in Google, return here, then refresh status",
                    ),
                )
            } catch (error: CancellationException) {
                publishOperationState(binding, presentation, previous.copy(isBusy = false))
                throw error
            } catch (error: Exception) {
                publishOperationState(
                    binding,
                    presentation,
                    failureState(error, previous.copy(isBusy = false)),
                )
            }
            } finally {
                bindingTicket.release()
            }
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
    ) {
        val presentation = beginPresentationOperation() ?: return
        operationMutex.withLock {
        if (!presentationOperationCurrent(presentation)) return@withLock
        val binding = credentialStore.snapshot()
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            publishPresentationState(
                presentation,
                GoogleAccountState(
                    phase = GoogleAccountPhase.AUTH_REQUIRED,
                    message = "Reconnect the DayWeave API before changing Google access",
                    requiresPlannerApiConfiguration = true,
                    configurationId = binding.configurationId,
                ),
            )
            return@withLock
        }
        if (configuration == null) {
            publishPresentationState(presentation, initialState(binding))
            return@withLock
        }
        if (!configurationMatchesBinding(configuration, binding, presentation)) return@withLock
        if (!operationStillCurrent(binding, presentation)) return@withLock
        val bindingTicket = try {
            configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            invalidatePresentationState(credentialStore.snapshot())
            return@withLock
        }
        try {
        if (!operationStillCurrent(binding, presentation)) return@withLock
        val previous = stateForBinding(binding, presentation) ?: return@withLock
        val account = previous.accounts.firstOrNull { it.id == accountId }
        if (account == null) {
            publishPresentationState(
                presentation,
                operationFailureStatePreservingRecovery(
                    previous,
                    "That Google account belongs to an older API connection · refresh status",
                ),
            )
            return@withLock
        }
        if (!publishPresentationState(presentation, previous.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = progressMessage,
        ))) return@withLock
        try {
            validateAccount(mutation(configuration, account))
            if (!operationStillCurrent(binding, presentation)) return@withLock
            val refreshed = transport.accounts(configuration)
            publishOperationState(
                binding,
                presentation,
                mapState(
                    refreshed,
                authorization = null,
                configurationId = binding.configurationId,
                ),
            )
        } catch (error: GoogleAccountsApiException.Unavailable) {
            if (!operationStillCurrent(binding, presentation)) return@withLock
            if (!reconcileUnavailable) {
                publishOperationState(
                    binding,
                    presentation,
                    failureState(error, previous.copy(isBusy = false)),
                )
                return@withLock
            }
            reconcileAmbiguousDisconnect(
                configuration = configuration,
                binding = binding,
                presentation = presentation,
                accountId = accountId,
                previous = previous,
                unavailable = true,
            )
        } catch (error: GoogleAccountsApiException.Conflict) {
            if (!operationStillCurrent(binding, presentation)) return@withLock
            try {
                val refreshed = transport.accounts(configuration)
                publishOperationState(
                    binding,
                    presentation,
                    mapState(
                        refreshed,
                        authorization = null,
                        configurationId = binding.configurationId,
                    ),
                )
            } catch (refreshError: CancellationException) {
                publishOperationState(
                    binding,
                    presentation,
                    operationFailureStatePreservingRecovery(
                        previous,
                        "Google account reconciliation was interrupted · refresh status",
                    ),
                )
                throw refreshError
            } catch (refreshError: Exception) {
                publishOperationState(
                    binding,
                    presentation,
                    failureState(refreshError, previous.copy(isBusy = false)),
                )
            }
        } catch (error: GoogleAccountsApiException.Http) {
            if (!operationStillCurrent(binding, presentation)) return@withLock
            if (reconcileUnavailable && error.statusCode == 404) {
                reconcileAmbiguousDisconnect(
                    configuration = configuration,
                    binding = binding,
                    presentation = presentation,
                    accountId = accountId,
                    previous = previous,
                    unavailable = false,
                )
            } else {
                publishOperationState(
                    binding,
                    presentation,
                    failureState(error, previous.copy(isBusy = false)),
                )
            }
        } catch (error: CancellationException) {
            publishOperationState(
                binding,
                presentation,
                operationFailureStatePreservingRecovery(
                    previous,
                    "Google update outcome is unknown · refresh status before retrying",
                ),
            )
            throw error
        } catch (error: Exception) {
            publishOperationState(
                binding,
                presentation,
                failureState(error, previous.copy(isBusy = false)),
            )
        }
        } finally {
            bindingTicket.release()
        }
        }
    }

    private suspend fun reconcileAmbiguousDisconnect(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
        presentation: Long,
        accountId: String,
        previous: GoogleAccountState,
        unavailable: Boolean,
    ) {
        try {
            val refreshed = transport.accounts(configuration)
            val mapped = mapState(
                refreshed,
                authorization = null,
                configurationId = binding.configurationId,
            )
            val reconciled = if (mapped.phase == GoogleAccountPhase.RECOVERY_REQUIRED) {
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
            publishOperationState(binding, presentation, reconciled)
        } catch (error: CancellationException) {
            publishOperationState(
                binding,
                presentation,
                operationFailureStatePreservingRecovery(
                    previous,
                    "Google disconnect outcome is unknown · refresh status before retrying",
                ),
            )
            throw error
        } catch (_: Exception) {
            publishOperationState(
                binding,
                presentation,
                operationFailureStatePreservingRecovery(
                    previous,
                    "Google access was not confirmed revoked · refresh status before retrying",
                ),
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

    private fun beginPresentationOperation(expectedGeneration: Long? = null): Long? =
        synchronized(presentationFence) {
            presentationGeneration.takeIf { generation ->
                operationAllowed() &&
                    (expectedGeneration == null || expectedGeneration == generation)
            }
        }

    private fun presentationOperationCurrent(generation: Long): Boolean =
        synchronized(presentationFence) {
            operationAllowed() && presentationGeneration == generation
        }

    private fun presentationState(generation: Long): GoogleAccountState? =
        synchronized(presentationFence) {
            mutableState.value.takeIf {
                operationAllowed() && presentationGeneration == generation
            }
        }

    /**
     * Publishes under the same monitor used by quarantine. Once quarantine returns, an older
     * generation can therefore never win a check-to-write race and restore private labels.
     */
    private fun publishPresentationState(
        generation: Long,
        next: GoogleAccountState,
    ): Boolean = synchronized(presentationFence) {
        if (!operationAllowed() || presentationGeneration != generation) return@synchronized false
        mutableState.value = next
        true
    }

    private fun operationStillCurrent(
        binding: ApiConnectionSnapshot,
        generation: Long,
    ): Boolean {
        val current = credentialStore.snapshot()
        if (!sameBinding(binding, current)) {
            invalidatePresentationState(current)
            return false
        }
        return presentationOperationCurrent(generation)
    }

    private fun publishOperationState(
        binding: ApiConnectionSnapshot,
        generation: Long,
        next: GoogleAccountState,
    ): Boolean {
        val current = credentialStore.snapshot()
        return synchronized(presentationFence) {
            if (!sameBinding(binding, current)) {
                invalidatePresentationStateLocked(current)
                return@synchronized false
            }
            if (!operationAllowed() || presentationGeneration != generation) {
                return@synchronized false
            }
            mutableState.value = next
            true
        }
    }

    private fun invalidatePresentationState(snapshot: ApiConnectionSnapshot) =
        synchronized(presentationFence) {
            invalidatePresentationStateLocked(snapshot)
        }

    private fun invalidatePresentationStateLocked(snapshot: ApiConnectionSnapshot) {
        presentationGeneration += 1
        val visibleSnapshot = snapshot.takeIf { operationAllowed() } ?: QUARANTINED_SNAPSHOT
        mutableState.value = initialState(visibleSnapshot)
    }

    private fun consumeAuthorizationIfCurrent(
        presentation: Long,
        candidate: String,
        binding: ApiConnectionSnapshot,
        currentBinding: ApiConnectionSnapshot,
        configurationMatches: Boolean,
        expectedConfigurationId: String?,
        consumer: (String) -> Unit,
    ): Boolean = synchronized(presentationFence) {
        if (!operationAllowed() || presentationGeneration != presentation) {
            return@synchronized false
        }
        val current = mutableState.value
        val authorization = current.authorization
        val trusted = sameBinding(binding, currentBinding) && binding.hasBearerToken &&
            configurationMatches && current.configurationId == expectedConfigurationId &&
            current.configurationId == binding.configurationId && authorization?.url == candidate &&
            authorization.expiresAt > now()
        if (!trusted) return@synchronized false
        consumer(candidate)
        true
    }

    private fun configurationMatchesBinding(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
        presentation: Long,
    ): Boolean {
        if (configurationMatchesBindingValue(configuration, binding)) {
            return presentationOperationCurrent(presentation)
        }
        invalidatePresentationState(credentialStore.snapshot())
        return false
    }

    private fun configurationMatchesBindingValue(
        configuration: com.greengolddog.dayweave.network.AuthenticatedApiConfiguration,
        binding: ApiConnectionSnapshot,
    ): Boolean =
        binding.hasBearerToken && binding.configurationId != null &&
            configuration.configurationId == binding.configurationId &&
            configuration.baseUrl.toString() == binding.baseUrl

    private fun stateForBinding(
        binding: ApiConnectionSnapshot,
        presentation: Long,
    ): GoogleAccountState? = synchronized(presentationFence) {
        if (!operationAllowed() || presentationGeneration != presentation) {
            return@synchronized null
        }
        mutableState.value.takeIf { state ->
            binding.hasBearerToken && state.configurationId != null &&
                state.configurationId == binding.configurationId
        } ?: initialState(binding)
    }

    private data class RestartAuthorization(
        val accountId: String?,
        val configurationId: String?,
        val presentationGeneration: Long,
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
        private val QUARANTINED_SNAPSHOT = ApiConnectionSnapshot(
            baseUrl = null,
            hasBearerToken = false,
            lastSuccessfulSyncEpochMillis = null,
            configurationId = null,
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
