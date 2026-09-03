package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.GoogleAccountsApiException
import com.greengolddog.dayweave.network.GoogleAccountsTransport
import com.greengolddog.dayweave.network.GoogleService
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
    AUTHORIZATION_RECOVERY,
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
    val action: GoogleAuthorizationAction,
    /** Existing account being repaired; null means a new Google account is being connected. */
    val accountId: String? = null,
) {
    override fun toString(): String =
        "PendingGoogleAuthorization(url=<redacted>, expiresAt=$expiresAt, " +
            "action=$action, target=${if (accountId == null) "new-account" else "existing-account"})"
}

/** Non-secret presentation of the durable exact-retry record. */
data class GoogleAuthorizationRecovery(
    val action: GoogleAuthorizationAction,
    val expiresAt: Instant,
    val accountId: String?,
    val browserOpened: Boolean,
    val belongsToCurrentConfiguration: Boolean,
    val browserWindowExpired: Boolean = false,
) {
    override fun toString(): String =
        "GoogleAuthorizationRecovery(action=$action, expiresAt=$expiresAt, " +
            "target=${if (accountId == null) "new-account" else "existing-account"}, " +
            "browserOpened=$browserOpened, browserWindowExpired=$browserWindowExpired, " +
            "currentBinding=$belongsToCurrentConfiguration)"
}

/** One-generation capability issued only for a currently surfaced unreadable-record warning. */
class GoogleAuthorizationRecoveryResetConfirmation internal constructor(
    internal val presentationGeneration: Long,
    internal val binding: ApiConnectionSnapshot,
    internal val expectedCorruptArtifact: GoogleAuthorizationCorruptArtifactIdentity,
) {
    override fun toString(): String = "GoogleAuthorizationRecoveryResetConfirmation(<redacted>)"
}

/** Exact one-generation capability for explicitly abandoning foreign/orphaned OAuth recovery. */
class GoogleAuthorizationRecoveryDiscardConfirmation internal constructor(
    internal val presentationGeneration: Long,
    internal val binding: ApiConnectionSnapshot,
    internal val expectedJournal: GoogleAuthorizationJournal?,
    internal val expectedCorruptArtifact: GoogleAuthorizationCorruptArtifactIdentity?,
) {
    override fun toString(): String = "GoogleAuthorizationRecoveryDiscardConfirmation(<redacted>)"
}

data class GoogleAccountState(
    val phase: GoogleAccountPhase,
    val accounts: List<GoogleAccountSummary> = emptyList(),
    val authorization: PendingGoogleAuthorization? = null,
    val message: String,
    val isBusy: Boolean = false,
    val requiresPlannerApiConfiguration: Boolean = false,
    /** Durable exact request retained without persisting the one-use Google URL. */
    val authorizationRecovery: GoogleAuthorizationRecovery? = null,
    /** An unreadable record blocks every new OAuth start until explicitly reset. */
    val authorizationRecoveryResetRequired: Boolean = false,
    /** Content-free signal for a foreign, orphaned, or unreadable record. */
    val authorizationRecoveryDiscardRequired: Boolean = false,
    /** Opaque API credential generation that owns every account and URL in this state. */
    val configurationId: String? = null,
)

class GoogleAccountManager(
    private val credentialStore: ApiCredentialStore,
    private val transport: GoogleAccountsTransport,
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
    private val operationAllowed: () -> Boolean = { true },
    /** Process-wide mutation admission; deliberately does not fence read-only [refresh]. */
    private val authorizationMutationAllowed: (
        action: GoogleAuthorizationAction,
        targetAccountId: String?,
    ) -> Boolean = { _, _ -> true },
    private val authorizationJournalStore: GoogleAuthorizationJournalStore =
        UnavailableGoogleAuthorizationJournalStore,
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

    /** Content-free synchronous admission signal for other process-wide mutation lanes. */
    fun hasAuthorizationRecoveryBlocker(): Boolean = try {
        val observedAt = now().toEpochMilli()
        when (val loaded = authorizationJournalStore.load(observedAt)) {
            GoogleAuthorizationJournalLoadResult.Empty -> false
            is GoogleAuthorizationJournalLoadResult.Loaded,
            is GoogleAuthorizationJournalLoadResult.Corrupt,
            is GoogleAuthorizationJournalLoadResult.Expired,
            -> true
            is GoogleAuthorizationJournalLoadResult.Retirable -> {
                !authorizationJournalStore.removeExact(loaded.journal, observedAt) ||
                    authorizationJournalStore.load(observedAt) !=
                    GoogleAuthorizationJournalLoadResult.Empty
            }
        }
    } catch (_: RuntimeException) {
        true
    }

    /** Atomically invalidates in-flight presentation work and drops all private provider data. */
    internal fun quarantineBindingState() {
        invalidatePresentationState(QUARANTINED_SNAPSHOT)
    }

    suspend fun refresh() {
        val presentation = beginPresentationOperation() ?: return
        operationMutex.withLock {
        if (!presentationOperationCurrent(presentation)) return@withLock
        val binding = credentialStore.snapshot()
        // Resolve the no-backup OAuth record before touching encrypted credentials. A lost or
        // foreign API binding must still be able to surface a content-free explicit discard path.
        val journalResolution = resolveAuthorizationJournal(binding)
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            publishPresentationState(
                presentation,
                configurationUnavailableState(
                    binding = binding,
                    journalResolution = journalResolution,
                    credentialsUnreadable = true,
                ),
            )
            return@withLock
        }
        if (configuration == null) {
            publishPresentationState(
                presentation,
                configurationUnavailableState(
                    binding = binding,
                    journalResolution = journalResolution,
                    credentialsUnreadable = false,
                ),
            )
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
        val journalBlock = journalResolution as? AuthorizationJournalResolution.Blocked
        val journal = (journalResolution as? AuthorizationJournalResolution.Available)?.journal
        val journalBelongsToBinding =
            (journalResolution as? AuthorizationJournalResolution.Available)
                ?.belongsToCurrentConfiguration ?: true
        // The authenticated configuration has now independently proved the binding metadata.
        // Never expose the saved action/account for a merely URL-matching foreign record.
        val recovery = journal?.takeIf { journalBelongsToBinding }?.toRecovery(true)
        val retainedAuthorization = previous.authorization?.takeIf { authorization ->
            journal != null && journalBelongsToBinding && !journal.browserOpened &&
                authorization.matches(journal) && authorization.expiresAt > now()
        }
        val recoveryBase = previous.copy(
            authorization = retainedAuthorization,
            authorizationRecovery = recovery,
            authorizationRecoveryResetRequired = false,
            authorizationRecoveryDiscardRequired = false,
        )
        val recoveryAwarePrevious = when {
            journalBlock != null -> authorizationRecoveryDiscardState(
                previous = recoveryBase,
                resetRequired = journalBlock.unreadable,
                message = journalBlock.message,
            )
            journal != null && !journalBelongsToBinding -> authorizationRecoveryDiscardState(
                previous = recoveryBase,
                resetRequired = false,
                message = FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE,
            )
            else -> recoveryBase
        }
        if (!publishPresentationState(presentation, recoveryAwarePrevious.copy(
            phase = GoogleAccountPhase.LOADING,
            isBusy = true,
            message = "Checking Google connection…",
        ))) return@withLock
        try {
            val response = transport.accounts(configuration)
            var mapped = mapState(
                response = response,
                authorization = retainedAuthorization,
                authorizationRecovery = recovery,
                configurationId = binding.configurationId,
            )
            if (journalBlock != null) {
                mapped = authorizationRecoveryDiscardState(
                    previous = mapped,
                    resetRequired = journalBlock.unreadable,
                    message = journalBlock.message,
                )
            } else if (journal != null && !journalBelongsToBinding) {
                mapped = authorizationRecoveryDiscardState(
                    previous = mapped,
                    resetRequired = false,
                    message = FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE,
                )
            }
            if (!publishOperationState(binding, presentation, mapped)) return@withLock
        } catch (error: CancellationException) {
            publishOperationState(binding, presentation, recoveryAwarePrevious.copy(isBusy = false))
            throw error
        } catch (error: Exception) {
            publishOperationState(
                binding,
                presentation,
                failureState(error, recoveryAwarePrevious.copy(isBusy = false)).let { failed ->
                    when {
                        journalBlock != null -> authorizationRecoveryDiscardState(
                            previous = failed,
                            resetRequired = journalBlock.unreadable,
                            message = journalBlock.message,
                        )
                        journal != null && !journalBelongsToBinding ->
                            authorizationRecoveryDiscardState(
                                previous = failed,
                                resetRequired = false,
                                message = FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE,
                            )
                        else -> failed
                    }
                },
            )
        }
        } finally {
            bindingTicket.release()
        }
        }
    }

    suspend fun connectNew() = beginAuthorization(accountId = null, service = null)

    suspend fun reauthorize(accountId: String) = beginAuthorization(accountId, service = null)

    /** Requests only the full Calendar scope for the selected existing account. */
    suspend fun enableCalendarPublishing(accountId: String) =
        beginAuthorization(accountId, service = GoogleService.CALENDAR)

    /** Requests only the full Tasks scope for the selected existing account. */
    suspend fun enableTasksPublishing(accountId: String) =
        beginAuthorization(accountId, service = GoogleService.TASKS)

    suspend fun restartAuthorization() {
        beginAuthorization(
            accountId = null,
            service = null,
            retrySavedRequest = true,
        )
    }

    fun unreadableAuthorizationRecoveryResetConfirmation():
        GoogleAuthorizationRecoveryResetConfirmation? {
        val binding = credentialStore.snapshot()
        val observedAt = try {
            now().toEpochMilli()
        } catch (_: RuntimeException) {
            return null
        }
        val corrupt = try {
            authorizationJournalStore.load(observedAt)
                as? GoogleAuthorizationJournalLoadResult.Corrupt
        } catch (_: RuntimeException) {
            null
        } ?: return null
        return synchronized(presentationFence) {
            mutableState.value.takeIf {
                operationAllowed() && it.authorizationRecoveryResetRequired && !it.isBusy &&
                    it.configurationId == binding.configurationId
            }?.let {
                GoogleAuthorizationRecoveryResetConfirmation(
                    presentationGeneration = presentationGeneration,
                    binding = binding,
                    expectedCorruptArtifact = corrupt.artifactIdentity,
                )
            }
        }
    }

    /** Explicitly removes only the unreadable record represented by [confirmation]. */
    suspend fun resetUnreadableAuthorizationRecovery(
        confirmation: GoogleAuthorizationRecoveryResetConfirmation,
    ) {
        val presentation = beginPresentationOperation(confirmation.presentationGeneration) ?: return
        operationMutex.withLock {
            val current = presentationState(presentation) ?: return@withLock
            val binding = credentialStore.snapshot()
            if (
                !current.authorizationRecoveryResetRequired || current.isBusy ||
                binding != confirmation.binding ||
                current.configurationId != binding.configurationId
            ) {
                return@withLock
            }
            val observedAt = try {
                now().toEpochMilli()
            } catch (_: RuntimeException) {
                return@withLock
            }
            val reset = try {
                val loaded = authorizationJournalStore.load(observedAt)
                loaded is GoogleAuthorizationJournalLoadResult.Corrupt &&
                    loaded.artifactIdentity == confirmation.expectedCorruptArtifact &&
                    authorizationRecoveryDestructionAllowed(presentation, confirmation.binding) &&
                    authorizationJournalStore.clearCorruptExact(
                        confirmation.expectedCorruptArtifact,
                        observedAt,
                    ) &&
                    authorizationJournalStore.load(observedAt) ==
                    GoogleAuthorizationJournalLoadResult.Empty
            } catch (_: RuntimeException) {
                false
            }
            if (reset) {
                invalidatePresentationState(binding)
            } else {
                publishPresentationState(
                    presentation,
                    current.copy(
                        phase = GoogleAccountPhase.ERROR,
                        isBusy = false,
                        message = "The saved Google authorization recovery could not be reset",
                    ),
                )
            }
        }
    }

    /**
     * Issues an opaque, one-presentation-generation capability for the exact recovery record and
     * exact non-secret credential snapshot currently shown as foreign or orphaned.
     */
    fun authorizationRecoveryDiscardConfirmation():
        GoogleAuthorizationRecoveryDiscardConfirmation? {
        val binding = credentialStore.snapshot()
        val observedAt = try {
            now().toEpochMilli()
        } catch (_: RuntimeException) {
            return null
        }
        val loaded = try {
            authorizationJournalStore.load(observedAt)
        } catch (_: RuntimeException) {
            return null
        }
        return synchronized(presentationFence) {
            val current = mutableState.value
            if (
                !operationAllowed() || current.isBusy ||
                !current.authorizationRecoveryDiscardRequired ||
                current.configurationId != binding.configurationId
            ) {
                return@synchronized null
            }
            when (loaded) {
                GoogleAuthorizationJournalLoadResult.Empty -> null
                is GoogleAuthorizationJournalLoadResult.Corrupt ->
                    GoogleAuthorizationRecoveryDiscardConfirmation(
                        presentationGeneration = presentationGeneration,
                        binding = binding,
                        expectedJournal = null,
                        expectedCorruptArtifact = loaded.artifactIdentity,
                    )
                is GoogleAuthorizationJournalLoadResult.Loaded ->
                    GoogleAuthorizationRecoveryDiscardConfirmation(
                        presentationGeneration = presentationGeneration,
                        binding = binding,
                        expectedJournal = loaded.journal,
                        expectedCorruptArtifact = null,
                    )
                is GoogleAuthorizationJournalLoadResult.Expired ->
                    GoogleAuthorizationRecoveryDiscardConfirmation(
                        presentationGeneration = presentationGeneration,
                        binding = binding,
                        expectedJournal = loaded.journal,
                        expectedCorruptArtifact = null,
                    )
                is GoogleAuthorizationJournalLoadResult.Retirable ->
                    GoogleAuthorizationRecoveryDiscardConfirmation(
                        presentationGeneration = presentationGeneration,
                        binding = binding,
                        expectedJournal = loaded.journal,
                        expectedCorruptArtifact = null,
                    )
            }
        }
    }

    /**
     * Explicitly abandons only the foreign/orphaned recovery represented by [confirmation].
     * Loaded and expired records use exact CAS removal; unreadable records use the distinct reset
     * primitive and must verify empty before the warning is retired.
     */
    suspend fun discardAuthorizationRecovery(
        confirmation: GoogleAuthorizationRecoveryDiscardConfirmation,
    ): Boolean {
        val presentation = beginPresentationOperation(confirmation.presentationGeneration)
            ?: return false
        return operationMutex.withLock {
            val current = presentationState(presentation) ?: return@withLock false
            val binding = credentialStore.snapshot()
            if (
                current.isBusy || !current.authorizationRecoveryDiscardRequired ||
                binding != confirmation.binding || current.configurationId != binding.configurationId
            ) {
                return@withLock false
            }
            val observedAt = try {
                now().toEpochMilli()
            } catch (_: RuntimeException) {
                return@withLock false
            }
            val loaded = try {
                authorizationJournalStore.load(observedAt)
            } catch (_: RuntimeException) {
                return@withLock false
            }
            val removed = when {
                confirmation.expectedCorruptArtifact != null ->
                    confirmation.expectedJournal == null &&
                        loaded is GoogleAuthorizationJournalLoadResult.Corrupt &&
                        loaded.artifactIdentity == confirmation.expectedCorruptArtifact &&
                        authorizationRecoveryDestructionAllowed(
                            presentation,
                            confirmation.binding,
                        ) &&
                        authorizationJournalStore.clearCorruptExact(
                            confirmation.expectedCorruptArtifact,
                            observedAt,
                        )
                confirmation.expectedJournal != null -> {
                    val currentJournal = when (loaded) {
                        is GoogleAuthorizationJournalLoadResult.Loaded -> loaded.journal
                        is GoogleAuthorizationJournalLoadResult.Expired -> loaded.journal
                        is GoogleAuthorizationJournalLoadResult.Retirable -> loaded.journal
                        GoogleAuthorizationJournalLoadResult.Empty,
                        is GoogleAuthorizationJournalLoadResult.Corrupt,
                        -> null
                    }
                    currentJournal == confirmation.expectedJournal &&
                        authorizationRecoveryDestructionAllowed(
                            presentation,
                            confirmation.binding,
                        ) &&
                        authorizationJournalStore.removeExact(
                            confirmation.expectedJournal,
                            observedAt,
                        )
                }
                else -> false
            }
            val verifiedEmpty = removed && try {
                authorizationJournalStore.load(observedAt) ==
                    GoogleAuthorizationJournalLoadResult.Empty
            } catch (_: RuntimeException) {
                false
            }
            if (!verifiedEmpty) return@withLock false
            invalidatePresentationState(binding)
            true
        }
    }

    /**
     * Destructive credential-removal fence used only after the owner confirmed local teardown.
     * Ordinary API binding replacement never calls this and remains blocked by any journal.
     */
    suspend fun abandonAuthorizationForConfirmedLocalDestruction(): Boolean =
        operationMutex.withLock {
            val observedAt = try {
                now().toEpochMilli()
            } catch (_: RuntimeException) {
                return@withLock false
            }
            if (!authorizationJournalStore.clearForConfirmedReset(observedAt)) {
                return@withLock false
            }
            if (authorizationJournalStore.load(observedAt) !=
                GoogleAuthorizationJournalLoadResult.Empty
            ) {
                return@withLock false
            }
            invalidatePresentationState(credentialStore.snapshot())
            true
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
            val handoffAt = now()
            val trusted = authorizationUrlIsCurrent(
                presentation = presentation,
                candidate = candidate,
                binding = binding,
                currentBinding = currentBinding,
                configurationMatches = bindingTicket != null && configuration != null &&
                    configurationMatchesBindingValue(configuration, binding),
                expectedConfigurationId = current.configurationId,
                handoffAt = handoffAt,
            )
            if (!trusted) {
                if (!sameBinding(binding, currentBinding)) {
                    publishPresentationState(presentation, initialState(currentBinding))
                } else if (
                    current.authorization?.let {
                        !authorizationMutationAllowed(it.action, it.accountId)
                    } == true
                ) {
                    publishPresentationState(
                        presentation,
                        current.copy(
                            phase = GoogleAccountPhase.ERROR,
                            message =
                                "Finish the current planner or Google recovery before opening authorization",
                        ),
                    )
                }
                return@withLock false
            }
            val loadResult = try {
                authorizationJournalStore.load(handoffAt.toEpochMilli())
            } catch (_: RuntimeException) {
                null
            }
            val journal = (loadResult as? GoogleAuthorizationJournalLoadResult.Loaded)?.journal
            if (
                journal == null || !journal.belongsTo(binding) || journal.browserOpened ||
                current.authorization?.matches(journal) != true
            ) {
                val failure = when {
                    loadResult == null ||
                        loadResult is GoogleAuthorizationJournalLoadResult.Corrupt ->
                        authorizationRecoveryDiscardState(
                            previous = current,
                            resetRequired = true,
                            message =
                                "The exact Google authorization recovery became unreadable · nothing was opened",
                        )
                    loadResult is GoogleAuthorizationJournalLoadResult.Expired ->
                        authorizationRecoveryState(
                            previous = current,
                            journal = loadResult.journal,
                            belongsToCurrentConfiguration = loadResult.journal.belongsTo(binding),
                            message =
                                "The Google browser window closed · waiting for any in-flight callback to settle",
                        )
                    loadResult is GoogleAuthorizationJournalLoadResult.Retirable ->
                        authorizationRecoveryState(
                            previous = current,
                            journal = loadResult.journal,
                            belongsToCurrentConfiguration = loadResult.journal.belongsTo(binding),
                            message = "The saved Google authorization is ready for safe cleanup",
                        )
                    journal != null && !journal.belongsTo(binding) ->
                        authorizationRecoveryDiscardState(
                            previous = current,
                            resetRequired = false,
                            message = FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE,
                        )
                    journal?.browserOpened == true -> authorizationRecoveryState(
                        previous = current,
                        journal = journal,
                        belongsToCurrentConfiguration = true,
                        message =
                            "Google was already opened for this exact request · refresh status",
                    )
                    else -> current.copy(
                        phase = GoogleAccountPhase.ERROR,
                        authorization = null,
                        authorizationRecovery = null,
                        authorizationRecoveryResetRequired = false,
                        authorizationRecoveryDiscardRequired = false,
                        isBusy = false,
                        message =
                            "The exact Google authorization recovery is unavailable · nothing was opened",
                    )
                }
                publishPresentationState(
                    presentation,
                    failure,
                )
                return@withLock false
            }
            val handoffEpochMillis = handoffAt.toEpochMilli()
            // A tolerated backward wall-clock adjustment must not make the durable marker violate
            // the server-created journal interval. Both values remain strictly before expiry.
            val durableOpenedAt = handoffEpochMillis.coerceAtLeast(journal.createdAtEpochMillis)
            val openedJournal = journal.recordingBrowserOpened(durableOpenedAt)
            if (!authorizationJournalStore.updateExact(
                    expected = journal,
                    replacement = openedJournal,
                    nowEpochMillis = handoffEpochMillis,
                )
            ) {
                publishPresentationState(
                    presentation,
                    current.copy(
                        phase = GoogleAccountPhase.ERROR,
                        authorization = null,
                        authorizationRecovery = journal.toRecovery(true),
                        authorizationRecoveryResetRequired = false,
                        isBusy = false,
                        message =
                            "Google was not opened because its browser handoff could not be saved",
                    ),
                )
                return@withLock false
            }
            val opened = consumeDurablyOpenedAuthorizationIfCurrent(
                presentation,
                candidate = candidate,
                binding = binding,
                currentBinding = credentialStore.snapshot(),
                expectedConfigurationId = current.configurationId,
                openedJournal = openedJournal,
                consumer = consumer,
            )
            if (!opened) {
                publishPresentationState(
                    presentation,
                    authorizationRecoveryState(
                        previous = current,
                        journal = openedJournal,
                        belongsToCurrentConfiguration = openedJournal.belongsTo(
                            credentialStore.snapshot(),
                        ),
                        message =
                            "Browser opening was blocked after its durable handoff marker · refresh status",
                    ),
                )
            }
            opened
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
        } else if (current.authorizationRecovery?.browserOpened == true) {
            publishPresentationState(
                presentation,
                current.copy(
                    phase = GoogleAccountPhase.AUTHORIZATION_RECOVERY,
                    message =
                        "Google browser opening is uncertain · refresh status; do not replay the request",
                ),
            )
        }
        }
    }

    private suspend fun beginAuthorization(
        accountId: String?,
        service: GoogleService?,
        retrySavedRequest: Boolean = false,
    ) {
        require(service == null || service == GoogleService.CALENDAR || service == GoogleService.TASKS)
        val requestedAction = when (service) {
            GoogleService.CALENDAR -> GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING
            GoogleService.TASKS -> GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING
            null -> if (accountId == null) {
                GoogleAuthorizationAction.CONNECT_READ_ONLY
            } else {
                GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY
            }
            else -> error("Unsupported Google authorization service")
        }
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
            if (previous.phase == GoogleAccountPhase.RECOVERY_REQUIRED) {
                publishPresentationState(
                    presentation,
                    previous.copy(
                        authorization = null,
                        isBusy = false,
                        message =
                            "Resolve Google credential cleanup recovery before changing authorization",
                    ),
                )
                return@withLock
            }
            val resolution = resolveAuthorizationJournal(binding)
            if (resolution is AuthorizationJournalResolution.Blocked) {
                publishPresentationState(
                    presentation,
                    authorizationRecoveryDiscardState(
                        previous = previous,
                        resetRequired = resolution.unreadable,
                        message = resolution.message,
                    ),
                )
                return@withLock
            }

            val loaded = resolution as? AuthorizationJournalResolution.Available
            val journal: GoogleAuthorizationJournal
            if (retrySavedRequest) {
                if (loaded == null) {
                    publishPresentationState(
                        presentation,
                        previous.copy(
                            phase = GoogleAccountPhase.ERROR,
                            authorization = null,
                            authorizationRecovery = null,
                            isBusy = false,
                            message = "There is no saved Google authorization request to retry",
                        ),
                    )
                    return@withLock
                }
                if (!loaded.belongsToCurrentConfiguration) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = loaded.journal,
                            belongsToCurrentConfiguration = false,
                            message =
                                "Restore the Planner API connection that owns this saved Google authorization",
                        ),
                    )
                    return@withLock
                }
                journal = loaded.journal
                if (!journal.isValidAt(now().toEpochMilli())) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "The Google browser window closed · waiting for any in-flight callback to settle",
                        ),
                    )
                    return@withLock
                }
                if (!authorizationMutationAllowed(journal.action, journal.request.accountId)) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "Finish the current planner or Google recovery before retrying authorization",
                        ),
                    )
                    return@withLock
                }
                if (journal.browserOpened) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "Google was already opened for this exact request · refresh status to verify it",
                        ),
                    )
                    return@withLock
                }
                if (
                    journal.request.accountId != null &&
                    previous.accounts.none { it.id == journal.request.accountId }
                ) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "The saved Google authorization target is not in the refreshed account list",
                        ),
                    )
                    return@withLock
                }
            } else {
                if (loaded != null) {
                    publishPresentationState(
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = loaded.journal,
                            belongsToCurrentConfiguration = loaded.belongsToCurrentConfiguration,
                            message =
                                "Finish or retry the exact saved Google authorization before starting another",
                        ),
                    )
                    return@withLock
                }
                if (!authorizationMutationAllowed(requestedAction, accountId)) {
                    publishPresentationState(
                        presentation,
                        previous.copy(
                            phase = GoogleAccountPhase.ERROR,
                            authorization = null,
                            isBusy = false,
                            message =
                                "Finish the current planner or Google recovery before changing authorization",
                        ),
                    )
                    return@withLock
                }
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
                if (account != null && !authorizationIsAllowed(account, service)) {
                    publishPresentationState(
                        presentation,
                        operationFailureStatePreservingRecovery(
                            previous,
                            "That Google account cannot grant the requested publishing access",
                        ),
                    )
                    return@withLock
                }
                if (account != null && authorizationAlreadySatisfied(account, service)) {
                    publishPresentationState(
                        presentation,
                        previous.copy(
                            phase = GoogleAccountPhase.CONNECTED,
                            authorization = null,
                            authorizationRecovery = null,
                            isBusy = false,
                            message = when (service) {
                                GoogleService.CALENDAR ->
                                    "Google Calendar publishing access is already enabled"
                                GoogleService.TASKS ->
                                    "Google Tasks publishing access is already enabled"
                                else -> "Google authorization is already current"
                            },
                        ),
                    )
                    return@withLock
                }
                val request = StartGoogleAuthorizationRequest(
                    // The explicit empty sentinel asks the server for Calendar and Tasks
                    // read-only. Full scopes are singleton existing-account upgrades.
                    services = service?.let(::listOf) ?: emptyList(),
                    forceConsent = account != null,
                    accountId = account?.id,
                    connectNew = account == null && previous.accounts.isNotEmpty(),
                    makeDefault = account?.isDefault ?: previous.accounts.none { it.isDefault },
                )
                val createdAt = now().toEpochMilli()
                journal = GoogleAuthorizationJournal(
                    configurationId = requireNotNull(binding.configurationId),
                    apiBaseUrl = requireNotNull(binding.baseUrl),
                    request = request,
                    idempotencyKey = newUuid().toString(),
                    createdAtEpochMillis = createdAt,
                    expiresAtEpochMillis = Math.addExact(
                        createdAt,
                        GoogleAuthorizationJournal.MAXIMUM_LIFETIME_MILLIS,
                    ),
                )
                if (!authorizationMutationAllowed(journal.action, journal.request.accountId)) {
                    publishPresentationState(
                        presentation,
                        previous.copy(
                            phase = GoogleAccountPhase.ERROR,
                            authorization = null,
                            isBusy = false,
                            message =
                                "Google authorization was not started because another recovery became active",
                        ),
                    )
                    return@withLock
                }
                if (!authorizationJournalStore.saveIfAbsent(journal, createdAt)) {
                    publishPresentationState(
                        presentation,
                        previous.copy(
                            phase = GoogleAccountPhase.ERROR,
                            authorization = null,
                            authorizationRecovery = null,
                            isBusy = false,
                            message =
                                "Google authorization was not sent because its recovery request could not be saved",
                        ),
                    )
                    return@withLock
                }
            }
            if (!publishPresentationState(presentation, previous.copy(
                phase = GoogleAccountPhase.LOADING,
                authorization = null,
                authorizationRecovery = journal.toRecovery(true),
                authorizationRecoveryResetRequired = false,
                isBusy = true,
                message = journal.action.preparationMessage(),
            ))) return@withLock
            try {
                if (
                    !operationStillCurrent(binding, presentation) ||
                    !authorizationMutationAllowed(journal.action, journal.request.accountId)
                ) {
                    publishOperationState(
                        binding,
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "The exact Google authorization was saved but another recovery now blocks sending it",
                        ),
                    )
                    return@withLock
                }
                val started = transport.startAuthorization(
                    configuration = configuration,
                    idempotencyKey = journal.idempotencyKey,
                    request = journal.request,
                )
                val expiresAt = Instant.parse(started.expiresAt)
                val responseObservedAt = now()
                require(
                    expiresAt > responseObservedAt &&
                        expiresAt <= responseObservedAt.plusSeconds(MAX_AUTHORIZATION_SECONDS),
                )
                validateGoogleAuthorizationUrl(started.authorizationUrl)
                val updatedJournal = journal.recordingServerExpiry(expiresAt.toEpochMilli())
                if (!authorizationJournalStore.updateExact(
                        expected = journal,
                        replacement = updatedJournal,
                        nowEpochMillis = responseObservedAt.toEpochMilli(),
                    )
                ) {
                    publishOperationState(
                        binding,
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = journal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "Google may have started authorization, but its refreshed recovery record could not be saved",
                        ),
                    )
                    return@withLock
                }
                if (
                    !operationStillCurrent(binding, presentation) ||
                    !authorizationMutationAllowed(
                        updatedJournal.action,
                        updatedJournal.request.accountId,
                    )
                ) {
                    publishOperationState(
                        binding,
                        presentation,
                        authorizationRecoveryState(
                            previous = previous,
                            journal = updatedJournal,
                            belongsToCurrentConfiguration = true,
                            message =
                                "Google authorization is saved, but another recovery blocks opening it",
                        ),
                    )
                    return@withLock
                }
                publishOperationState(
                    binding,
                    presentation,
                    previous.copy(
                        phase = GoogleAccountPhase.AWAITING_BROWSER,
                        authorization = PendingGoogleAuthorization(
                            url = started.authorizationUrl,
                            expiresAt = expiresAt,
                            action = updatedJournal.action,
                            accountId = updatedJournal.request.accountId,
                        ),
                        authorizationRecovery = updatedJournal.toRecovery(true),
                        authorizationRecoveryResetRequired = false,
                        isBusy = false,
                        message = updatedJournal.action.browserReadyMessage(),
                    ),
                )
            } catch (error: CancellationException) {
                publishOperationState(
                    binding,
                    presentation,
                    authorizationStartFailureState(previous, journal, error),
                )
                throw error
            } catch (error: Exception) {
                publishOperationState(
                    binding,
                    presentation,
                    authorizationStartFailureState(previous, journal, error),
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
        when (val authorizationJournal = resolveAuthorizationJournal(binding)) {
            AuthorizationJournalResolution.None -> Unit
            is AuthorizationJournalResolution.Available -> {
                publishPresentationState(
                    presentation,
                    authorizationRecoveryState(
                        previous = previous,
                        journal = authorizationJournal.journal,
                        belongsToCurrentConfiguration =
                            authorizationJournal.belongsToCurrentConfiguration,
                        message =
                            "Finish the saved Google authorization before changing this account",
                    ),
                )
                return@withLock
            }
            is AuthorizationJournalResolution.Blocked -> {
                publishPresentationState(
                    presentation,
                    authorizationRecoveryDiscardState(
                        previous = previous,
                        resetRequired = authorizationJournal.unreadable,
                        message = authorizationJournal.message,
                    ),
                )
                return@withLock
            }
        }
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
            if (hasAuthorizationRecoveryBlocker()) {
                publishOperationState(
                    binding,
                    presentation,
                    previous.copy(
                        phase = GoogleAccountPhase.ERROR,
                        authorization = null,
                        isBusy = false,
                        message =
                            "A Google authorization recovery became active before the account change",
                    ),
                )
                return@withLock
            }
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

    private fun resolveAuthorizationJournal(
        binding: ApiConnectionSnapshot,
    ): AuthorizationJournalResolution {
        return try {
            val observedAt = now().toEpochMilli()
            when (val loaded = authorizationJournalStore.load(observedAt)) {
                GoogleAuthorizationJournalLoadResult.Empty -> AuthorizationJournalResolution.None
                is GoogleAuthorizationJournalLoadResult.Corrupt ->
                    AuthorizationJournalResolution.Blocked(
                        message = UNREADABLE_AUTHORIZATION_RECOVERY_MESSAGE,
                        unreadable = true,
                    )
                is GoogleAuthorizationJournalLoadResult.Expired ->
                    AuthorizationJournalResolution.Available(
                        journal = loaded.journal,
                        belongsToCurrentConfiguration = loaded.journal.belongsTo(binding),
                    )
                is GoogleAuthorizationJournalLoadResult.Retirable -> {
                    if (
                        authorizationJournalStore.removeExact(loaded.journal, observedAt) &&
                        authorizationJournalStore.load(observedAt) ==
                        GoogleAuthorizationJournalLoadResult.Empty
                    ) {
                        AuthorizationJournalResolution.None
                    } else {
                        AuthorizationJournalResolution.Blocked(
                            message = EXPIRED_AUTHORIZATION_RECOVERY_MESSAGE,
                            unreadable = false,
                        )
                    }
                }
                is GoogleAuthorizationJournalLoadResult.Loaded ->
                    AuthorizationJournalResolution.Available(
                        journal = loaded.journal,
                        belongsToCurrentConfiguration = loaded.journal.belongsTo(binding),
                    )
            }
        } catch (_: RuntimeException) {
            AuthorizationJournalResolution.Blocked(
                message = UNREADABLE_AUTHORIZATION_RECOVERY_MESSAGE,
                unreadable = true,
            )
        }
    }

    private fun configurationUnavailableState(
        binding: ApiConnectionSnapshot,
        journalResolution: AuthorizationJournalResolution,
        credentialsUnreadable: Boolean,
    ): GoogleAccountState {
        val base = if (credentialsUnreadable) {
            GoogleAccountState(
                phase = GoogleAccountPhase.AUTH_REQUIRED,
                message = "Planner credentials are unavailable · reconnect the DayWeave API",
                requiresPlannerApiConfiguration = true,
                configurationId = binding.configurationId,
            )
        } else {
            initialState(binding)
        }
        return when (journalResolution) {
            AuthorizationJournalResolution.None -> base
            is AuthorizationJournalResolution.Available -> authorizationRecoveryDiscardState(
                previous = base,
                resetRequired = false,
                message = ORPHANED_AUTHORIZATION_RECOVERY_MESSAGE,
            ).copy(requiresPlannerApiConfiguration = true)
            is AuthorizationJournalResolution.Blocked -> authorizationRecoveryDiscardState(
                previous = base,
                resetRequired = journalResolution.unreadable,
                message = journalResolution.message,
            ).copy(requiresPlannerApiConfiguration = true)
        }
    }

    private fun GoogleAuthorizationJournal.belongsTo(binding: ApiConnectionSnapshot): Boolean =
        binding.hasBearerToken && configurationId == binding.configurationId &&
            apiBaseUrl == binding.baseUrl

    private fun GoogleAuthorizationJournal.toRecovery(
        belongsToCurrentConfiguration: Boolean,
    ): GoogleAuthorizationRecovery = GoogleAuthorizationRecovery(
        action = action,
        expiresAt = Instant.ofEpochMilli(expiresAtEpochMillis),
        accountId = request.accountId,
        browserOpened = browserOpened,
        belongsToCurrentConfiguration = belongsToCurrentConfiguration,
        browserWindowExpired = expiresAtEpochMillis <= now().toEpochMilli(),
    )

    private fun PendingGoogleAuthorization.matches(journal: GoogleAuthorizationJournal): Boolean =
        action == journal.action && accountId == journal.request.accountId &&
            expiresAt.toEpochMilli() == journal.expiresAtEpochMillis

    private fun authorizationIsAllowed(
        account: GoogleAccountSummary,
        service: GoogleService?,
    ): Boolean = when (service) {
        GoogleService.CALENDAR -> account.status in setOf("active", "paused")
        GoogleService.TASKS, null -> account.status in setOf(
            "active",
            "paused",
            "reauthorization_required",
        )
        else -> false
    }

    private fun authorizationAlreadySatisfied(
        account: GoogleAccountSummary,
        service: GoogleService?,
    ): Boolean = when (service) {
        GoogleService.CALENDAR -> account.hasCalendarWriteScope
        GoogleService.TASKS ->
            account.hasTasksWriteScope && account.status != "reauthorization_required"
        else -> false
    }

    private fun authorizationRecoveryState(
        previous: GoogleAccountState,
        journal: GoogleAuthorizationJournal,
        belongsToCurrentConfiguration: Boolean,
        message: String,
    ): GoogleAccountState = if (belongsToCurrentConfiguration) {
        previous.copy(
            phase = GoogleAccountPhase.AUTHORIZATION_RECOVERY,
            authorization = null,
            authorizationRecovery = journal.toRecovery(true),
            authorizationRecoveryResetRequired = false,
            authorizationRecoveryDiscardRequired = false,
            isBusy = false,
            message = message,
        )
    } else {
        authorizationRecoveryDiscardState(
            previous = previous,
            resetRequired = false,
            message = FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE,
        )
    }

    private fun authorizationRecoveryDiscardState(
        previous: GoogleAccountState,
        resetRequired: Boolean,
        message: String,
    ): GoogleAccountState = previous.copy(
        phase = if (resetRequired) GoogleAccountPhase.ERROR else {
            GoogleAccountPhase.AUTHORIZATION_RECOVERY
        },
        authorization = null,
        authorizationRecovery = null,
        authorizationRecoveryResetRequired = resetRequired,
        authorizationRecoveryDiscardRequired = true,
        isBusy = false,
        message = message,
    )

    private fun authorizationStartFailureState(
        previous: GoogleAccountState,
        journal: GoogleAuthorizationJournal,
        error: Exception,
    ): GoogleAccountState {
        val authenticationFailed = error is GoogleAccountsApiException.Authentication
        return previous.copy(
            phase = if (authenticationFailed) {
                GoogleAccountPhase.AUTH_REQUIRED
            } else {
                GoogleAccountPhase.AUTHORIZATION_RECOVERY
            },
            accounts = if (authenticationFailed) emptyList() else previous.accounts,
            authorization = null,
            authorizationRecovery = journal.toRecovery(true),
            authorizationRecoveryResetRequired = false,
            authorizationRecoveryDiscardRequired = false,
            isBusy = false,
            message = when (error) {
                is GoogleAccountsApiException.Authentication ->
                    "Reconnect the Planner API, then retry the exact saved Google authorization"
                is GoogleAccountsApiException.Conflict ->
                    "Google authorization is already changing · keep and retry the exact saved request"
                is GoogleAccountsApiException.Validation ->
                    "Google rejected the request · the exact saved authorization was retained"
                is CancellationException, is IOException ->
                    "Google authorization outcome is unknown · retry only the exact saved request"
                else ->
                    "Google authorization was not confirmed · the exact saved request was retained"
            },
            requiresPlannerApiConfiguration = authenticationFailed,
        )
    }

    private fun GoogleAuthorizationAction.preparationMessage(): String = when (this) {
        GoogleAuthorizationAction.CONNECT_READ_ONLY -> "Preparing a private Google connection…"
        GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY ->
            "Preparing private Google reauthorization…"
        GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING ->
            "Preparing Google Calendar publishing access…"
        GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING ->
            "Preparing Google Tasks publishing access…"
    }

    private fun GoogleAuthorizationAction.browserReadyMessage(): String = when (this) {
        GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING ->
            "Authorize Calendar publishing in Google, then refresh status"
        GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING ->
            "Authorize Tasks publishing in Google, then refresh status"
        else -> "Authorize in Google, return here, then refresh status"
    }

    private fun GoogleAuthorizationAction.retryMessage(): String = when (this) {
        GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING ->
            "Retry the exact saved Calendar publishing authorization"
        GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING ->
            "Retry the exact saved Tasks publishing authorization"
        GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY ->
            "Retry the exact saved Google reauthorization"
        GoogleAuthorizationAction.CONNECT_READ_ONLY ->
            "Retry the exact saved read-only Google connection"
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
        authorizationRecovery: GoogleAuthorizationRecovery? = null,
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
                authorizationRecovery = authorizationRecovery,
                message = if (operatorRecovery) {
                    "Google credential recovery needs owner attention on the server"
                } else {
                    "Google credential cleanup is fenced · the server will retry safely"
                },
            )
            authorization != null && authorizationRecovery != null -> GoogleAccountState(
                phase = GoogleAccountPhase.AWAITING_BROWSER,
                accounts = accounts,
                authorization = authorization,
                authorizationRecovery = authorizationRecovery,
                message = authorizationRecovery.action.browserReadyMessage(),
            )
            authorizationRecovery != null -> GoogleAccountState(
                phase = GoogleAccountPhase.AUTHORIZATION_RECOVERY,
                accounts = accounts,
                authorizationRecovery = authorizationRecovery,
                message = when {
                    !authorizationRecovery.belongsToCurrentConfiguration ->
                        "Restore the Planner API connection that owns this saved Google authorization"
                    authorizationRecovery.browserWindowExpired ->
                        "The Google browser window closed · waiting for any in-flight callback to settle"
                    authorizationRecovery.browserOpened ->
                        "Google was opened for the saved request · refresh to verify the grant"
                    else -> authorizationRecovery.action.retryMessage()
                },
            )
            accounts.any { it.status == "active" } -> GoogleAccountState(
                phase = GoogleAccountPhase.CONNECTED,
                accounts = accounts,
                message = when {
                    revocationFailed ->
                        "Google connected · another account still needs Disconnect retried"
                    needsAuthorization ->
                        "Google connected · one or more accounts need authorization"
                    else -> "Google Calendar and Tasks connected"
                },
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
        val accountId = UUID.fromString(remote.id)
        require(accountId.toString() == remote.id && accountId != UUID(0, 0))
        require(remote.externalAccountId.isSafeLabel() && remote.displayLabel.isSafeLabel())
        // Leave room for the server's mandatory next revision.
        require(remote.status in GOOGLE_ACCOUNT_STATUSES && remote.revision in 1 until Long.MAX_VALUE)
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

    /** Final privacy/generation/binding fence immediately before an explicit destructive clear. */
    private fun authorizationRecoveryDestructionAllowed(
        presentation: Long,
        expectedBinding: ApiConnectionSnapshot,
    ): Boolean = credentialStore.snapshot() == expectedBinding &&
        presentationOperationCurrent(presentation)

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

    private fun authorizationUrlIsCurrent(
        presentation: Long,
        candidate: String,
        binding: ApiConnectionSnapshot,
        currentBinding: ApiConnectionSnapshot,
        configurationMatches: Boolean,
        expectedConfigurationId: String?,
        handoffAt: Instant,
    ): Boolean = synchronized(presentationFence) {
        if (!operationAllowed() || presentationGeneration != presentation) {
            return@synchronized false
        }
        val current = mutableState.value
        val authorization = current.authorization
        if (
            authorization == null ||
            !authorizationMutationAllowed(authorization.action, authorization.accountId)
        ) {
            return@synchronized false
        }
        val trusted = sameBinding(binding, currentBinding) && binding.hasBearerToken &&
            configurationMatches && current.configurationId == expectedConfigurationId &&
            current.configurationId == binding.configurationId && authorization?.url == candidate &&
            authorization.expiresAt > handoffAt
        trusted
    }

    /** Final fence check and synchronous handoff after the opened marker is durable. */
    private fun consumeDurablyOpenedAuthorizationIfCurrent(
        presentation: Long,
        candidate: String,
        binding: ApiConnectionSnapshot,
        currentBinding: ApiConnectionSnapshot,
        expectedConfigurationId: String?,
        openedJournal: GoogleAuthorizationJournal,
        consumer: (String) -> Unit,
    ): Boolean = synchronized(presentationFence) {
        if (
            !operationAllowed() || !authorizationMutationAllowed(
                openedJournal.action,
                openedJournal.request.accountId,
            ) ||
            presentationGeneration != presentation
        ) {
            return@synchronized false
        }
        val current = mutableState.value
        val authorization = current.authorization
        val trusted = sameBinding(binding, currentBinding) && binding.hasBearerToken &&
            openedJournal.belongsTo(binding) &&
            current.configurationId == expectedConfigurationId &&
            current.configurationId == binding.configurationId &&
            authorization?.url == candidate && authorization.matches(openedJournal)
        if (!trusted) return@synchronized false
        mutableState.value = current.copy(
            phase = GoogleAccountPhase.AUTHORIZATION_RECOVERY,
            authorization = null,
            authorizationRecovery = openedJournal.toRecovery(true),
            authorizationRecoveryResetRequired = false,
            isBusy = false,
            message =
                "Google browser handoff is recorded · return and refresh to verify the grant",
        )
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

    private sealed interface AuthorizationJournalResolution {
        data object None : AuthorizationJournalResolution

        data class Available(
            val journal: GoogleAuthorizationJournal,
            val belongsToCurrentConfiguration: Boolean,
        ) : AuthorizationJournalResolution

        data class Blocked(
            val message: String,
            val unreadable: Boolean,
        ) : AuthorizationJournalResolution
    }

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
        private const val FOREIGN_AUTHORIZATION_RECOVERY_MESSAGE =
            "A saved Google authorization belongs to another API connection and may have " +
                "reached Google · restore that exact connection or explicitly discard it"
        private const val ORPHANED_AUTHORIZATION_RECOVERY_MESSAGE =
            "A saved Google authorization may have reached Google · restore its exact Planner " +
                "API connection or explicitly discard it"
        private const val UNREADABLE_AUTHORIZATION_RECOVERY_MESSAGE =
            "A saved Google authorization recovery is unreadable and may have reached Google · " +
                "explicitly discard it before reconnecting"
        private const val EXPIRED_AUTHORIZATION_RECOVERY_MESSAGE =
            "An expired Google authorization recovery could not be cleared safely and may have " +
                "reached Google · explicitly discard it"
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
