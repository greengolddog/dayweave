package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ConfigureGoogleCollectionRequest
import com.greengolddog.dayweave.network.GoogleCalendarInboundApiException
import com.greengolddog.dayweave.network.GoogleCalendarInboundTransport
import com.greengolddog.dayweave.network.GoogleInboundCollectionRole
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncCollection
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunStatus
import com.greengolddog.dayweave.network.RemoteGoogleSyncStatus
import com.greengolddog.dayweave.network.hasSupportedInboundRole
import java.io.IOException
import java.time.Instant
import java.time.format.DateTimeParseException
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class GoogleCalendarImportPhase {
    NOT_CONFIGURED,
    READY,
    LOADING_COLLECTIONS,
    DISCOVERING_COLLECTIONS,
    CONFIGURING_COLLECTION,
    PREPARING_REFRESH,
    REQUESTING_REFRESH,
    RESPONSE_UNKNOWN,
    CHECKING_COMPLETION,
    SERVER_BACKOFF,
    PERSISTING_CANONICAL_RESULT,
    COMPLETED,
    AUTH_REQUIRED,
    OFFLINE,
    RECOVERY_REQUIRED,
    ERROR,
}

data class GoogleImportCollectionState(
    val id: String,
    val accountId: String,
    val displayName: String,
    val kind: RemoteGoogleCollectionKind,
    val providerDeleted: Boolean,
    val selected: Boolean,
    val visible: Boolean,
    val syncRole: RemoteGoogleSyncRole,
    val calendarPolicy: RemoteGoogleCalendarPolicy,
    val revision: Long,
    val lastImportAt: String?,
    /** Provider authority is required to prove an outbound Calendar target is owner/writer. */
    val providerAccessRole: String? = null,
) {
    /** Calendar names and server identifiers are private even though they are not credentials. */
    override fun toString(): String =
        "GoogleImportCollectionState(identity=<redacted>, label=<redacted>, kind=$kind, " +
            "providerDeleted=$providerDeleted, selected=$selected, visible=$visible, " +
            "syncRole=$syncRole, revision=$revision)"
}

data class GoogleImportRunState(
    val state: RemoteGoogleSyncRunState,
    val refreshGeneration: Long,
    val claimedRefreshGeneration: Long,
    val completedRefreshGeneration: Long,
    val nextAttemptAt: Instant,
    val importedCount: Long,
    val updatedCount: Long,
    val deletedCount: Long,
    val conflictCount: Long,
    val rejectedCount: Long,
)

data class GoogleImportAccountState(
    val collections: List<GoogleImportCollectionState> = emptyList(),
    val run: GoogleImportRunState? = null,
) {
    override fun toString(): String =
        "GoogleImportAccountState(collectionCount=${collections.size}, run=$run)"
}

data class GoogleCalendarImportState(
    val phase: GoogleCalendarImportPhase,
    val message: String,
    val isBusy: Boolean = false,
    /** Exact account IDs are keys for UI routing, but [toString] never emits them. */
    val accounts: Map<String, GoogleImportAccountState> = emptyMap(),
    val activeAccountId: String? = null,
    val acceptedRefreshGeneration: Long? = null,
    val pollAttempt: Int = 0,
    val pendingRecoveryCount: Int = 0,
    /** Exact IDs route recovery controls; diagnostics expose only their count. */
    val pendingRecoveryAccountIds: Set<String> = emptySet(),
    val configurationId: String? = null,
) {
    override fun toString(): String =
        "GoogleCalendarImportState(phase=$phase, isBusy=$isBusy, " +
            "accountCount=${accounts.size}, activeAccount=<redacted>, " +
            "acceptedRefreshGeneration=$acceptedRefreshGeneration, pollAttempt=$pollAttempt, " +
            "pendingRecoveryCount=$pendingRecoveryCount, pendingAccountCount=" +
            "${pendingRecoveryAccountIds.size}, configuration=<redacted>)"
}

enum class GoogleCalendarImportOutcome {
    COMPLETED,
    PENDING,
    RESPONSE_UNKNOWN,
    NO_PENDING_REQUEST,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    RECOVERY_REQUIRED,
    FAILED,
}

enum class GoogleImportCollectionsOutcome {
    LOADED,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    OFFLINE,
    RECOVERY_REQUIRED,
    FAILED,
}

enum class GoogleImportConfigurationOutcome {
    CONFIGURED,
    RECONCILED,
    CONFLICT,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    OFFLINE,
    RECOVERY_REQUIRED,
    FAILED,
}

/**
 * Finite delay schedule. A coordinator call never polls or reconciles beyond this list.
 */
data class GoogleCalendarImportRetryPolicy(
    val delaysMillis: List<Long> = listOf(0L, 2_000L, 4_000L, 8_000L, 16_000L),
) {
    init {
        require(delaysMillis.isNotEmpty())
        require(delaysMillis.size <= MAX_ATTEMPTS)
        require(delaysMillis.first() == 0L)
        require(delaysMillis.all { it in 0..MAX_SINGLE_DELAY_MILLIS })
        require(
            delaysMillis.fold(0L) { total, next -> Math.addExact(total, next) } <=
                MAX_TOTAL_DELAY_MILLIS,
        )
    }

    private companion object {
        const val MAX_ATTEMPTS = 16
        const val MAX_SINGLE_DELAY_MILLIS = 60_000L
        const val MAX_TOTAL_DELAY_MILLIS = 5 * 60_000L
    }
}

data class GoogleCalendarImportCompletionInput(
    val configurationId: String,
    val apiBaseUrl: String,
    val accountId: String,
    val acceptedRefreshGeneration: Long,
) {
    override fun toString(): String =
        "GoogleCalendarImportCompletionInput(binding=<redacted>, account=<redacted>, " +
            "acceptedRefreshGeneration=$acceptedRefreshGeneration)"
}

/** Returned only after canonical refresh, local composition, publication, and their save finish. */
data class GoogleCalendarImportPersistenceReceipt(
    val configurationId: String,
    val apiBaseUrl: String,
    val accountId: String,
    val completedRefreshGeneration: Long,
    val durablyPersisted: Boolean,
) {
    override fun toString(): String =
        "GoogleCalendarImportPersistenceReceipt(binding=<redacted>, account=<redacted>, " +
            "completedRefreshGeneration=$completedRefreshGeneration, " +
            "durablyPersisted=$durablyPersisted)"
}

fun interface GoogleCalendarImportCompletionPipeline {
    /**
     * Refreshes canonical state, composes, publishes, and returns only after the result is durable.
     * A false or mismatched receipt retains the import journal for safe replay.
     */
    suspend fun persistCanonicalRefreshCompositionAndPublication(
        input: GoogleCalendarImportCompletionInput,
    ): GoogleCalendarImportPersistenceReceipt
}

/**
 * Crash-safe Google Calendar import orchestration shared by foreground and restart recovery.
 *
 * A request UUID is durably written before the refresh POST. Its accepted generation is written
 * immediately after the exact 202 response, and completion requires an authoritative idle run at
 * or beyond that generation. The marker remains until the injected canonical pipeline returns an
 * exact durable receipt.
 */
class GoogleCalendarImportCoordinator(
    private val credentialStore: ApiCredentialStore,
    private val transport: GoogleCalendarInboundTransport,
    private val journalStore: GoogleCalendarImportJournalStore,
    private val completionPipeline: GoogleCalendarImportCompletionPipeline,
    private val retryPolicy: GoogleCalendarImportRetryPolicy = GoogleCalendarImportRetryPolicy(),
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
    private val newRequestId: () -> UUID = UUID::randomUUID,
    private val sleep: suspend (Long) -> Unit = { delay(it) },
    private val operationAllowed: () -> Boolean = { true },
    private val importAllowed: () -> Boolean = { true },
) {
    private val operationMutex = Mutex()

    /**
     * Serializes only lifecycle generation changes and in-memory presentation publication.
     * Credential and journal reads must finish before this monitor is entered.
     */
    private val presentationMonitor = Any()
    private val lifecycleGeneration = AtomicLong(1)
    private val mutableState = MutableStateFlow(stateAfterQuarantine(credentialStore.snapshot()))
    val state: StateFlow<GoogleCalendarImportState> = mutableState.asStateFlow()

    /**
     * Drops every cached account ID and label at the credential writer boundary.
     * Durable journals stay intact and remain bound to the credential generation that created them.
     */
    fun quarantineBindingState() {
        val quarantined = stateAfterQuarantine(credentialStore.snapshot())
        synchronized(presentationMonitor) {
            lifecycleGeneration.updateAndGet { current ->
                Math.addExact(current, 1L)
            }
            mutableState.value = quarantined
        }
    }

    fun hasCredentialRecoveryBlocker(): Boolean = when (
        val loaded = journalStore.load(safeNow())
    ) {
        is GoogleCalendarImportJournalLoadResult.Loaded -> loaded.journals.isNotEmpty()
        GoogleCalendarImportJournalLoadResult.Corrupt -> true
    }

    /**
     * Destructively abandons every non-secret import marker only for an explicitly confirmed local
     * credential destruction flow. Ordinary binding changes must call [hasCredentialRecoveryBlocker]
     * and leave these records untouched.
     */
    suspend fun abandonPendingForConfirmedLocalDestruction(): Boolean = operationMutex.withLock {
        val abandoned = journalStore.abandonAllForConfirmedLocalDestruction(safeNow())
        if (abandoned) {
            quarantineBindingState()
        } else {
            val lifecycle = lifecycleGeneration.get()
            val snapshot = credentialStore.snapshot()
            setState(
                lifecycle,
                initialState(snapshot).copy(
                    phase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
                    message = JOURNAL_ABANDONMENT_FAILED,
                    pendingRecoveryCount = pendingCount(),
                ),
            )
        }
        abandoned
    }

    suspend fun loadCollections(accountId: String): GoogleImportCollectionsOutcome =
        collectionsOperation(accountId, discover = false)

    suspend fun discoverCollections(accountId: String): GoogleImportCollectionsOutcome =
        collectionsOperation(accountId, discover = true)

    suspend fun configureCollection(
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ): GoogleImportConfigurationOutcome {
        val lifecycle = lifecycleGeneration.get()
        val binding = authenticatedBinding(lifecycle)
            ?: return configurationBindingFailure()
        val bindingTicket = try {
            binding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            resetAfterBindingChange(lifecycle)
            return GoogleImportConfigurationOutcome.RECOVERY_REQUIRED
        }
        return try {
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                val pendingImport = journalStore.load(safeNow())
                if (
                    pendingImport !is GoogleCalendarImportJournalLoadResult.Loaded ||
                    pendingImport.journals.isNotEmpty()
                ) {
                    updateRecoveryFailure(
                        lifecycle,
                        binding,
                        CONFIGURATION_IMPORT_PENDING,
                        pendingCount(),
                    )
                    return@withLock GoogleImportConfigurationOutcome.RECOVERY_REQUIRED
                }
                if (!accountId.isCanonicalGoogleUuid() || !collectionId.isCanonicalGoogleUuid()) {
                    updateFailure(
                        lifecycle,
                        binding,
                        GoogleCalendarImportPhase.ERROR,
                        CONFIGURATION_INVALID,
                    )
                    return@withLock GoogleImportConfigurationOutcome.FAILED
                }
                if (
                    request.expectedRevision !in 1 until Long.MAX_VALUE ||
                    !request.hasSupportedInboundRole ||
                    !request.calendarPolicy.isInboundOnly
                ) {
                    updateFailure(
                        lifecycle,
                        binding,
                        GoogleCalendarImportPhase.ERROR,
                        CONFIGURATION_INVALID,
                    )
                    return@withLock GoogleImportConfigurationOutcome.FAILED
                }
                val previous = stateFor(binding)
                val authoritativeMatches = previous.accounts[accountId]
                    ?.collections
                    ?.filter { collection ->
                        collection.accountId == accountId && collection.id == collectionId
                    }
                    .orEmpty()
                val cachedAuthoritative = authoritativeMatches.singleOrNull()
                if (
                    cachedAuthoritative == null ||
                    cachedAuthoritative.revision != request.expectedRevision ||
                    cachedAuthoritative.kind != request.kind ||
                    cachedAuthoritative.providerDeleted ||
                    cachedAuthoritative.syncRole == RemoteGoogleSyncRole.WRITABLE
                ) {
                    updateFailure(
                        lifecycle,
                        binding,
                        GoogleCalendarImportPhase.ERROR,
                        CONFIGURATION_INVALID,
                    )
                    return@withLock GoogleImportConfigurationOutcome.FAILED
                }
                setState(
                    lifecycle,
                    previous.copy(
                        phase = GoogleCalendarImportPhase.CONFIGURING_COLLECTION,
                        message = CONFIGURATION_SAVING,
                        isBusy = true,
                        activeAccountId = accountId,
                    ),
                )
                val updated = try {
                    transport.configure(
                        configuration = binding.configuration,
                        accountId = accountId,
                        collectionId = collectionId,
                        request = request,
                    ).also {
                        validateConfiguredCollection(it, accountId, collectionId, request)
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    if (!error.requiresAuthoritativeConfigurationRead()) throw error
                    val authoritative = reconcileConfiguration(
                        lifecycle = lifecycle,
                        binding = binding,
                        accountId = accountId,
                        collectionId = collectionId,
                        request = request,
                    )
                    if (authoritative != null) {
                        installCollections(lifecycle, binding, accountId, authoritative.second)
                        setState(
                            lifecycle,
                            mutableState.value.copy(
                                phase = GoogleCalendarImportPhase.READY,
                                message = CONFIGURATION_RECONCILED,
                                isBusy = false,
                                activeAccountId = accountId,
                            ),
                        )
                        return@withLock GoogleImportConfigurationOutcome.RECONCILED
                    }
                    throw ConfigurationNotReconciledException(error)
                }
                requireCurrent(lifecycle, binding)
                replaceCollection(lifecycle, binding, updated)
                setState(
                    lifecycle,
                    mutableState.value.copy(
                        phase = GoogleCalendarImportPhase.READY,
                        message = CONFIGURATION_SAVED,
                        isBusy = false,
                        activeAccountId = accountId,
                    ),
                )
                GoogleImportConfigurationOutcome.CONFIGURED
            }
        } catch (error: CancellationException) {
            restoreAfterCancellation(lifecycle, binding)
            throw error
        } catch (_: ApiBindingChangedException) {
            resetAfterBindingChange(lifecycle)
            GoogleImportConfigurationOutcome.RECOVERY_REQUIRED
        } catch (_: StaleGoogleImportOperationException) {
            resetAfterBindingChange(lifecycle)
            GoogleImportConfigurationOutcome.RECOVERY_REQUIRED
        } catch (error: ConfigurationNotReconciledException) {
            val kind = classifyRemoteFailure(error.cause ?: error)
            val phase = when (kind) {
                RemoteFailureKind.AUTHENTICATION -> GoogleCalendarImportPhase.AUTH_REQUIRED
                RemoteFailureKind.OFFLINE, RemoteFailureKind.AMBIGUOUS,
                RemoteFailureKind.RETRYABLE,
                -> GoogleCalendarImportPhase.OFFLINE
                else -> GoogleCalendarImportPhase.ERROR
            }
            val message = if (kind == RemoteFailureKind.CONFLICT) {
                CONFIGURATION_CONFLICT
            } else {
                CONFIGURATION_UNRESOLVED
            }
            updateFailure(lifecycle, binding, phase, message)
            when (kind) {
                RemoteFailureKind.CONFLICT -> GoogleImportConfigurationOutcome.CONFLICT
                RemoteFailureKind.AUTHENTICATION -> GoogleImportConfigurationOutcome.AUTH_REQUIRED
                RemoteFailureKind.OFFLINE, RemoteFailureKind.AMBIGUOUS,
                RemoteFailureKind.RETRYABLE,
                -> GoogleImportConfigurationOutcome.OFFLINE
                else -> GoogleImportConfigurationOutcome.FAILED
            }
        } catch (error: Exception) {
            configurationFailure(lifecycle, binding, error)
        } finally {
            bindingTicket.release()
        }
    }

    suspend fun refresh(accountId: String): GoogleCalendarImportOutcome {
        if (!importAllowed()) return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        return refreshInternal(accountId, allowNewRequest = true)
    }

    suspend fun recoverPending(accountId: String): GoogleCalendarImportOutcome {
        if (!importAllowed()) return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        return refreshInternal(accountId, allowNewRequest = false)
    }

    private suspend fun collectionsOperation(
        accountId: String,
        discover: Boolean,
    ): GoogleImportCollectionsOutcome {
        val lifecycle = lifecycleGeneration.get()
        val binding = authenticatedBinding(lifecycle) ?: return collectionsBindingFailure()
        val bindingTicket = try {
            binding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            resetAfterBindingChange(lifecycle)
            return GoogleImportCollectionsOutcome.RECOVERY_REQUIRED
        }
        return try {
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                if (!accountId.isCanonicalGoogleUuid()) {
                    updateFailure(
                        lifecycle,
                        binding,
                        GoogleCalendarImportPhase.ERROR,
                        COLLECTIONS_INVALID,
                    )
                    return@withLock GoogleImportCollectionsOutcome.FAILED
                }
                setState(
                    lifecycle,
                    stateFor(binding).copy(
                        phase = if (discover) {
                            GoogleCalendarImportPhase.DISCOVERING_COLLECTIONS
                        } else {
                            GoogleCalendarImportPhase.LOADING_COLLECTIONS
                        },
                        message = if (discover) COLLECTIONS_DISCOVERING else COLLECTIONS_LOADING,
                        isBusy = true,
                        activeAccountId = accountId,
                    ),
                )
                val remote = if (discover) {
                    transport.discover(binding.configuration, accountId)
                } else {
                    transport.collections(binding.configuration, accountId)
                }
                requireCurrent(lifecycle, binding)
                val collections = validateCollections(remote.collections, accountId)
                installCollections(lifecycle, binding, accountId, collections)
                setState(
                    lifecycle,
                    mutableState.value.copy(
                        phase = GoogleCalendarImportPhase.READY,
                        message = if (discover) COLLECTIONS_DISCOVERED else COLLECTIONS_LOADED,
                        isBusy = false,
                        activeAccountId = accountId,
                    ),
                )
                GoogleImportCollectionsOutcome.LOADED
            }
        } catch (error: CancellationException) {
            restoreAfterCancellation(lifecycle, binding)
            throw error
        } catch (_: ApiBindingChangedException) {
            resetAfterBindingChange(lifecycle)
            GoogleImportCollectionsOutcome.RECOVERY_REQUIRED
        } catch (_: StaleGoogleImportOperationException) {
            resetAfterBindingChange(lifecycle)
            GoogleImportCollectionsOutcome.RECOVERY_REQUIRED
        } catch (error: Exception) {
            collectionsFailure(lifecycle, binding, error)
        } finally {
            bindingTicket.release()
        }
    }

    private suspend fun refreshInternal(
        accountId: String,
        allowNewRequest: Boolean,
    ): GoogleCalendarImportOutcome {
        if (!importAllowed()) return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        val lifecycle = lifecycleGeneration.get()
        val binding = authenticatedBinding(lifecycle) ?: return bindingFailureOutcome()
        val bindingTicket = try {
            binding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            retainRecoveryAfterBindingChange(lifecycle)
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        var completionJournal: GoogleCalendarImportJournal? = null
        val networkOutcome = try {
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                if (!accountId.isCanonicalGoogleUuid()) {
                    updateFailure(lifecycle, binding, GoogleCalendarImportPhase.ERROR, REFRESH_INVALID)
                    return@withLock GoogleCalendarImportOutcome.FAILED
                }
                val loaded = journalStore.load(safeNow())
                if (loaded !is GoogleCalendarImportJournalLoadResult.Loaded) {
                    updateRecoveryFailure(lifecycle, binding, JOURNAL_UNREADABLE)
                    return@withLock GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                }
                val foreign = loaded.journals.filterNot {
                    it.configurationId == binding.configurationId &&
                        it.apiBaseUrl == binding.apiBaseUrl
                }
                if (foreign.isNotEmpty()) {
                    updateRecoveryFailure(
                        lifecycle,
                        binding,
                        JOURNAL_OTHER_BINDING,
                        loaded.journals.size,
                    )
                    return@withLock GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                }
                var journal = loaded.journals.firstOrNull { it.accountId == accountId }
                var isFreshlyPersistedFirstSend = false
                if (journal == null && !allowNewRequest) {
                    setState(
                        lifecycle,
                        stateFor(binding).copy(
                            phase = GoogleCalendarImportPhase.READY,
                            message = NO_PENDING_REFRESH,
                            isBusy = false,
                            activeAccountId = accountId,
                            pendingRecoveryCount = loaded.journals.size,
                        ),
                    )
                    return@withLock GoogleCalendarImportOutcome.NO_PENDING_REQUEST
                }
                if (journal == null) {
                    val preparedAt = safeNow()
                    journal = try {
                        GoogleCalendarImportJournal(
                            configurationId = binding.configurationId,
                            apiBaseUrl = binding.apiBaseUrl,
                            accountId = accountId,
                            requestId = newRequestId().toString(),
                            createdAtEpochMillis = preparedAt,
                        )
                    } catch (_: Exception) {
                        updateRecoveryFailure(lifecycle, binding, JOURNAL_NOT_SAVED)
                        return@withLock GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                    }
                    setState(
                        lifecycle,
                        stateFor(binding).copy(
                            phase = GoogleCalendarImportPhase.PREPARING_REFRESH,
                            message = REFRESH_PREPARING,
                            isBusy = true,
                            activeAccountId = accountId,
                            pendingRecoveryCount = loaded.journals.size,
                        ),
                    )
                    if (!journalStore.save(journal, preparedAt)) {
                        updateRecoveryFailure(lifecycle, binding, JOURNAL_NOT_SAVED)
                        return@withLock GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                    }
                    isFreshlyPersistedFirstSend = true
                }
                val durableJournal = requireNotNull(journal)
                val acceptedJournal = if (durableJournal.isAccepted) {
                    durableJournal
                } else {
                    requestAndRecordAcceptance(
                        lifecycle,
                        binding,
                        durableJournal,
                        isFreshlyPersistedFirstSend = isFreshlyPersistedFirstSend,
                    ) ?: return@withLock currentRefreshOutcome()
                }
                reconcileAcceptedImport(
                    lifecycle = lifecycle,
                    binding = binding,
                    journal = acceptedJournal,
                    allowTerminalRestart = allowNewRequest && durableJournal.isAccepted,
                    onCanonicalCompletion = { completed ->
                        completionJournal = completed
                        GoogleCalendarImportOutcome.PENDING
                    },
                )
            }
        } catch (error: CancellationException) {
            val hasRecovery = when (val loaded = journalStore.load(safeNow())) {
                is GoogleCalendarImportJournalLoadResult.Loaded -> loaded.journals.any {
                    it.configurationId == binding.configurationId && it.accountId == accountId
                }
                GoogleCalendarImportJournalLoadResult.Corrupt -> true
            }
            if (hasRecovery) {
                retainRecoveryAfterCancellation(lifecycle, binding, accountId)
            } else {
                restoreAfterCancellation(lifecycle, binding)
            }
            throw error
        } catch (error: ApiBindingChangedException) {
            retainRecoveryAfterBindingChange(lifecycle)
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        } catch (_: StaleGoogleImportOperationException) {
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        } catch (error: Exception) {
            refreshFailure(lifecycle, binding, error)
        } finally {
            bindingTicket.release()
        }
        return completionJournal?.let { completed ->
            persistCanonicalCompletion(lifecycle, binding, completed)
        } ?: networkOutcome
    }

    /**
     * [isFreshlyPersistedFirstSend] is provenance, not a property of [journal]. A prepared record
     * loaded from disk may already have been accepted before its 202 response was lost.
     */
    private suspend fun requestAndRecordAcceptance(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        journal: GoogleCalendarImportJournal,
        isFreshlyPersistedFirstSend: Boolean,
    ): GoogleCalendarImportJournal? {
        requireCurrent(lifecycle, binding)
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.REQUESTING_REFRESH,
                message = REFRESH_REQUESTING,
                isBusy = true,
                activeAccountId = journal.accountId,
                pendingRecoveryCount = pendingCount(),
            ),
        )
        val accepted = try {
            transport.refresh(
                configuration = binding.configuration,
                accountId = journal.accountId,
                requestId = UUID.fromString(journal.requestId),
            )
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            requireCurrent(lifecycle, binding)
            val kind = classifyRemoteFailure(error)
            if (
                isFreshlyPersistedFirstSend &&
                kind.isDefinitivePreAcceptanceRejection()
            ) {
                val retired = journalStore.retireRejectedPreparedExact(journal, safeNow())
                if (retired) {
                    setState(
                        lifecycle,
                        stateFor(binding).copy(
                            phase = GoogleCalendarImportPhase.ERROR,
                            message = REFRESH_REJECTED,
                            isBusy = false,
                            activeAccountId = journal.accountId,
                            acceptedRefreshGeneration = null,
                            pollAttempt = 0,
                            pendingRecoveryCount = pendingCount(),
                        ),
                    )
                } else {
                    updateRecoveryFailure(
                        lifecycle,
                        binding,
                        REFRESH_REJECTED_RECOVERY_RETAINED,
                        pendingCount(),
                    )
                }
                return null
            }
            val phase = when (kind) {
                RemoteFailureKind.AUTHENTICATION -> GoogleCalendarImportPhase.AUTH_REQUIRED
                RemoteFailureKind.AMBIGUOUS, RemoteFailureKind.OFFLINE,
                RemoteFailureKind.RETRYABLE,
                -> GoogleCalendarImportPhase.RESPONSE_UNKNOWN
                else -> GoogleCalendarImportPhase.RECOVERY_REQUIRED
            }
            setState(
                lifecycle,
                stateFor(binding).copy(
                    phase = phase,
                    message = when (phase) {
                        GoogleCalendarImportPhase.RESPONSE_UNKNOWN -> REFRESH_RESPONSE_UNKNOWN
                        GoogleCalendarImportPhase.AUTH_REQUIRED -> REFRESH_AUTH_REQUIRED
                        else -> REFRESH_REJECTED_RECOVERY_RETAINED
                    },
                    isBusy = false,
                    activeAccountId = journal.accountId,
                    pendingRecoveryCount = pendingCount(),
                ),
            )
            return null
        }
        requireCurrent(lifecycle, binding)
        if (
            accepted.accountId != journal.accountId ||
            accepted.requestId != journal.requestId ||
            accepted.refreshGeneration !in 1 until Long.MAX_VALUE ||
            runCatching { Instant.parse(accepted.requestedAt) }.isFailure
        ) {
            throw InvalidGoogleImportResponseException()
        }
        val recordedAt = safeNow().coerceAtLeast(journal.createdAtEpochMillis)
        val acceptedJournal = journal.recordingAcceptance(
            refreshGeneration = accepted.refreshGeneration,
            recordedAtEpochMillis = recordedAt,
        )
        if (!journalStore.save(acceptedJournal, recordedAt)) {
            updateRecoveryFailure(lifecycle, binding, ACCEPTANCE_NOT_SAVED, pendingCount())
            return null
        }
        requireCurrent(lifecycle, binding)
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.CHECKING_COMPLETION,
                message = REFRESH_ACCEPTED,
                isBusy = true,
                activeAccountId = journal.accountId,
                acceptedRefreshGeneration = accepted.refreshGeneration,
                pendingRecoveryCount = pendingCount(),
            ),
        )
        return acceptedJournal
    }

    private suspend fun reconcileAcceptedImport(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        journal: GoogleCalendarImportJournal,
        allowTerminalRestart: Boolean,
        onCanonicalCompletion: (GoogleCalendarImportJournal) -> GoogleCalendarImportOutcome,
    ): GoogleCalendarImportOutcome {
        val acceptedGeneration = requireNotNull(journal.acceptedRefreshGeneration)
        for ((attemptIndex, delayMillis) in retryPolicy.delaysMillis.withIndex()) {
            if (delayMillis > 0) sleep(delayMillis)
            requireCurrent(lifecycle, binding)
            setState(
                lifecycle,
                stateFor(binding).copy(
                    phase = GoogleCalendarImportPhase.CHECKING_COMPLETION,
                    message = REFRESH_CHECKING,
                    isBusy = true,
                    activeAccountId = journal.accountId,
                    acceptedRefreshGeneration = acceptedGeneration,
                    pollAttempt = attemptIndex + 1,
                    pendingRecoveryCount = pendingCount(),
                ),
            )
            val status = try {
                transport.syncStatus(binding.configuration, journal.accountId)
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                requireCurrent(lifecycle, binding)
                when (classifyRemoteFailure(error)) {
                    RemoteFailureKind.AUTHENTICATION -> {
                        updateFailure(
                            lifecycle,
                            binding,
                            GoogleCalendarImportPhase.AUTH_REQUIRED,
                            REFRESH_AUTH_REQUIRED,
                        )
                        return GoogleCalendarImportOutcome.AUTH_REQUIRED
                    }

                    RemoteFailureKind.OFFLINE -> {
                        updateFailure(
                            lifecycle,
                            binding,
                            GoogleCalendarImportPhase.OFFLINE,
                            REFRESH_OFFLINE,
                        )
                        return GoogleCalendarImportOutcome.PENDING
                    }

                    RemoteFailureKind.AMBIGUOUS, RemoteFailureKind.RETRYABLE -> {
                        if (attemptIndex + 1 < retryPolicy.delaysMillis.size) continue
                        setRefreshPending(lifecycle, binding, journal, REFRESH_CHECK_LATER)
                        return GoogleCalendarImportOutcome.PENDING
                    }

                    else -> {
                        updateRecoveryFailure(lifecycle, binding, REFRESH_STATUS_INVALID, pendingCount())
                        return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                    }
                }
            }
            requireCurrent(lifecycle, binding)
            val run = validateSyncStatus(status, journal.accountId)
            installRun(lifecycle, binding, journal.accountId, run)
            if (
                run != null &&
                run.state == RemoteGoogleSyncRunState.IDLE &&
                run.completedRefreshGeneration >= acceptedGeneration
            ) {
                prepareCanonicalCompletion(lifecycle, binding, journal)
                return onCanonicalCompletion(journal)
            }
            when (run?.state) {
                RemoteGoogleSyncRunState.BACKOFF -> {
                    setState(
                        lifecycle,
                        stateFor(binding).copy(
                            phase = GoogleCalendarImportPhase.SERVER_BACKOFF,
                            message = REFRESH_SERVER_BACKOFF,
                            isBusy = false,
                            activeAccountId = journal.accountId,
                            acceptedRefreshGeneration = acceptedGeneration,
                            pollAttempt = attemptIndex + 1,
                            pendingRecoveryCount = pendingCount(),
                        ),
                    )
                    return GoogleCalendarImportOutcome.PENDING
                }

                RemoteGoogleSyncRunState.REAUTHORIZATION_REQUIRED -> {
                    if (allowTerminalRestart) {
                        return restartTerminalImport(
                            lifecycle,
                            binding,
                            journal,
                            onCanonicalCompletion,
                        )
                    }
                    updateFailure(
                        lifecycle,
                        binding,
                        GoogleCalendarImportPhase.AUTH_REQUIRED,
                        REFRESH_AUTH_REQUIRED,
                    )
                    return GoogleCalendarImportOutcome.AUTH_REQUIRED
                }

                RemoteGoogleSyncRunState.FAILED -> {
                    if (allowTerminalRestart) {
                        return restartTerminalImport(
                            lifecycle,
                            binding,
                            journal,
                            onCanonicalCompletion,
                        )
                    }
                    updateRecoveryFailure(lifecycle, binding, REFRESH_FAILED, pendingCount())
                    return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                }

                RemoteGoogleSyncRunState.IDLE, RemoteGoogleSyncRunState.RUNNING, null -> Unit
            }
        }
        setRefreshPending(lifecycle, binding, journal, REFRESH_CHECK_LATER)
        return GoogleCalendarImportOutcome.PENDING
    }

    /** The authoritative terminal run is the only fact allowed to retire an accepted UUID. */
    private suspend fun restartTerminalImport(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        terminal: GoogleCalendarImportJournal,
        onCanonicalCompletion: (GoogleCalendarImportJournal) -> GoogleCalendarImportOutcome,
    ): GoogleCalendarImportOutcome {
        val restartedAt = safeNow().coerceAtLeast(
            requireNotNull(terminal.acceptedRecordedAtEpochMillis),
        )
        val replacement = try {
            GoogleCalendarImportJournal(
                configurationId = terminal.configurationId,
                apiBaseUrl = terminal.apiBaseUrl,
                accountId = terminal.accountId,
                requestId = newRequestId().toString(),
                createdAtEpochMillis = restartedAt,
            )
        } catch (_: Exception) {
            updateRecoveryFailure(lifecycle, binding, TERMINAL_RESTART_NOT_SAVED, pendingCount())
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.PREPARING_REFRESH,
                message = TERMINAL_RESTART_PREPARING,
                isBusy = true,
                activeAccountId = terminal.accountId,
                acceptedRefreshGeneration = null,
                pollAttempt = 0,
                pendingRecoveryCount = pendingCount(),
            ),
        )
        if (!journalStore.restartAcceptedExact(terminal, replacement, restartedAt)) {
            updateRecoveryFailure(lifecycle, binding, TERMINAL_RESTART_NOT_SAVED, pendingCount())
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        requireCurrent(lifecycle, binding)
        val accepted = requestAndRecordAcceptance(
            lifecycle,
            binding,
            replacement,
            isFreshlyPersistedFirstSend = true,
        )
            ?: return currentRefreshOutcome()
        return reconcileAcceptedImport(
            lifecycle = lifecycle,
            binding = binding,
            journal = accepted,
            allowTerminalRestart = false,
            onCanonicalCompletion = onCanonicalCompletion,
        )
    }

    private fun prepareCanonicalCompletion(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        journal: GoogleCalendarImportJournal,
    ) {
        val acceptedGeneration = requireNotNull(journal.acceptedRefreshGeneration)
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.PERSISTING_CANONICAL_RESULT,
                message = CANONICAL_PERSISTING,
                isBusy = true,
                activeAccountId = journal.accountId,
                acceptedRefreshGeneration = acceptedGeneration,
                pendingRecoveryCount = pendingCount(),
            ),
        )
    }

    /** Runs canonical API work without holding either the import mutex or an outer reader ticket. */
    private suspend fun persistCanonicalCompletion(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        journal: GoogleCalendarImportJournal,
    ): GoogleCalendarImportOutcome {
        val acceptedGeneration = requireNotNull(journal.acceptedRefreshGeneration)
        val currentSnapshot = credentialStore.snapshot()
        val completionAllowed = synchronized(presentationMonitor) {
            operationAllowed() && importAllowed() && lifecycleGeneration.get() == lifecycle &&
                sameBinding(currentSnapshot, binding.snapshot)
        }
        if (!completionAllowed) return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        val receipt = try {
            completionPipeline.persistCanonicalRefreshCompositionAndPublication(
                GoogleCalendarImportCompletionInput(
                    configurationId = binding.configurationId,
                    apiBaseUrl = binding.apiBaseUrl,
                    accountId = journal.accountId,
                    acceptedRefreshGeneration = acceptedGeneration,
                ),
            )
        } catch (error: CancellationException) {
            retainRecoveryAfterCancellation(lifecycle, binding, journal.accountId)
            throw error
        } catch (_: Exception) {
            updateRecoveryFailure(lifecycle, binding, CANONICAL_NOT_PERSISTED, pendingCount())
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        if (
            !receipt.durablyPersisted ||
            receipt.configurationId != binding.configurationId ||
            receipt.apiBaseUrl != binding.apiBaseUrl ||
            receipt.accountId != journal.accountId ||
            receipt.completedRefreshGeneration < acceptedGeneration
        ) {
            updateRecoveryFailure(lifecycle, binding, CANONICAL_NOT_PERSISTED, pendingCount())
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        if (
            !operationAllowed() || !importAllowed() || lifecycleGeneration.get() != lifecycle ||
            !sameBinding(credentialStore.snapshot(), binding.snapshot)
        ) {
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        val currentBinding = authenticatedBinding(lifecycle)
            ?: return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        if (
            currentBinding.configurationId != binding.configurationId ||
            currentBinding.apiBaseUrl != binding.apiBaseUrl
        ) {
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        val bindingTicket = try {
            currentBinding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            retainRecoveryAfterBindingChange(lifecycle)
            return GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
        return try {
            operationMutex.withLock {
                requireCurrent(lifecycle, currentBinding)
                val removed = journalStore.removeExact(journal, safeNow())
                if (!removed) {
                    updateRecoveryFailure(
                        lifecycle,
                        currentBinding,
                        COMPLETION_NOT_CLEARED,
                        pendingCount(),
                    )
                    return@withLock GoogleCalendarImportOutcome.RECOVERY_REQUIRED
                }
                setState(
                    lifecycle,
                    stateFor(currentBinding).copy(
                        phase = GoogleCalendarImportPhase.COMPLETED,
                        message = REFRESH_COMPLETED,
                        isBusy = false,
                        activeAccountId = journal.accountId,
                        acceptedRefreshGeneration = acceptedGeneration,
                        pollAttempt = 0,
                        pendingRecoveryCount = pendingCount(),
                    ),
                )
                GoogleCalendarImportOutcome.COMPLETED
            }
        } catch (error: CancellationException) {
            retainRecoveryAfterCancellation(lifecycle, currentBinding, journal.accountId)
            throw error
        } catch (_: ApiBindingChangedException) {
            retainRecoveryAfterBindingChange(lifecycle)
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        } catch (_: StaleGoogleImportOperationException) {
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        } finally {
            bindingTicket.release()
        }
    }

    private suspend fun reconcileConfiguration(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ): Pair<RemoteGoogleSyncCollection, List<RemoteGoogleSyncCollection>>? {
        for ((attemptIndex, delayMillis) in retryPolicy.delaysMillis.withIndex()) {
            if (delayMillis > 0) sleep(delayMillis)
            requireCurrent(lifecycle, binding)
            val collections = try {
                validateCollections(
                    transport.collections(binding.configuration, accountId).collections,
                    accountId,
                )
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                when (classifyRemoteFailure(error)) {
                    RemoteFailureKind.AMBIGUOUS, RemoteFailureKind.OFFLINE,
                    RemoteFailureKind.RETRYABLE,
                    -> if (attemptIndex + 1 < retryPolicy.delaysMillis.size) continue else throw error

                    else -> throw error
                }
            }
            requireCurrent(lifecycle, binding)
            val candidate = collections.firstOrNull { it.id == collectionId } ?: return null
            return if (
                candidate.revision > request.expectedRevision &&
                candidate.matches(request)
            ) {
                candidate to collections
            } else {
                null
            }
        }
        return null
    }

    private fun validateConfiguredCollection(
        collection: RemoteGoogleSyncCollection,
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ) {
        try {
            validateCollection(collection, accountId)
        } catch (cause: IllegalArgumentException) {
            throw GoogleCalendarInboundApiException.InvalidResponse(cause)
        }
        if (
            collection.id != collectionId ||
            collection.revision != request.expectedRevision + 1 ||
            !collection.matches(request)
        ) {
            throw GoogleCalendarInboundApiException.InvalidResponse()
        }
    }

    private fun validateCollections(
        collections: List<RemoteGoogleSyncCollection>,
        accountId: String,
    ): List<RemoteGoogleSyncCollection> {
        require(collections.size <= MAX_COLLECTIONS)
        collections.forEach { validateCollection(it, accountId) }
        require(collections.map { it.id }.toSet().size == collections.size)
        return collections.sortedWith(compareBy({ it.displayName.lowercase() }, { it.id }))
    }

    private fun validateCollection(collection: RemoteGoogleSyncCollection, accountId: String) {
        require(collection.accountId == accountId)
        require(collection.accountId.isCanonicalGoogleUuid())
        require(collection.id.isCanonicalGoogleUuid())
        require(collection.displayName.length in 1..MAX_COLLECTION_LABEL_LENGTH)
        require(collection.revision in 1 until Long.MAX_VALUE)
        require(
            collection.kind != RemoteGoogleCollectionKind.TASK_LIST ||
                collection.syncRole != RemoteGoogleSyncRole.BLOCKING,
        )
        require(
            collection.syncRole == RemoteGoogleSyncRole.WRITABLE ||
                collection.calendarPolicy.isInboundOnly,
        )
        // Another client (for example macOS) may have configured WRITABLE. Android displays that
        // authoritative state but its inbound-only ConfigureGoogleCollectionRequest can never send it.
    }

    private fun validateSyncStatus(
        status: RemoteGoogleSyncStatus,
        accountId: String,
    ): RemoteGoogleSyncRunStatus? {
        require(status.importConflicts >= 0)
        require(status.pendingOutbound >= 0)
        require(status.conflictedOutbound >= 0)
        require(status.failedOutbound >= 0)
        val run = status.run ?: return null
        require(run.accountId == accountId && run.accountId.isCanonicalGoogleUuid())
        require(run.consecutiveFailures >= 0)
        require(run.importedCount >= 0)
        require(run.updatedCount >= 0)
        require(run.deletedCount >= 0)
        require(run.conflictCount >= 0)
        require(run.rejectedCount >= 0)
        require(run.refreshGeneration >= 0)
        require(run.claimedRefreshGeneration in 0..run.refreshGeneration)
        require(run.completedRefreshGeneration in 0..run.refreshGeneration)
        require(run.revision in 1 until Long.MAX_VALUE)
        Instant.parse(run.nextAttemptAt)
        run.requestedAt?.let(Instant::parse)
        run.startedAt?.let(Instant::parse)
        run.completedAt?.let(Instant::parse)
        run.lastErrorAt?.let(Instant::parse)
        status.lastOutboundErrorAt?.let(Instant::parse)
        status.nextOutboundAttemptAt?.let(Instant::parse)
        return run
    }

    private fun replaceCollection(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        updated: RemoteGoogleSyncCollection,
    ) {
        val current = stateFor(binding)
        val account = current.accounts[updated.accountId] ?: GoogleImportAccountState()
        val collections = account.collections
            .filterNot { it.id == updated.id }
            .plus(updated.toState())
            .sortedWith(compareBy({ it.displayName.lowercase() }, { it.id }))
        setState(
            lifecycle,
            current.copy(
                accounts = current.accounts +
                    (updated.accountId to account.copy(collections = collections)),
            ),
        )
    }

    private fun installCollections(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        accountId: String,
        collections: List<RemoteGoogleSyncCollection>,
    ) {
        val current = stateFor(binding)
        val account = current.accounts[accountId] ?: GoogleImportAccountState()
        setState(
            lifecycle,
            current.copy(
                accounts = current.accounts +
                    (accountId to account.copy(collections = collections.map { it.toState() })),
            ),
        )
    }

    private fun installRun(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        accountId: String,
        run: RemoteGoogleSyncRunStatus?,
    ) {
        val current = stateFor(binding)
        val account = current.accounts[accountId] ?: GoogleImportAccountState()
        setState(
            lifecycle,
            current.copy(
                accounts = current.accounts + (accountId to account.copy(run = run?.toState())),
            ),
        )
    }

    private fun authenticatedBinding(lifecycle: Long): BoundGoogleImportConfiguration? {
        if (!operationAllowed()) return null
        val snapshot = credentialStore.snapshot()
        if (!snapshot.hasBearerToken || snapshot.configurationId == null || snapshot.baseUrl == null) {
            setState(lifecycle, initialState(snapshot))
            return null
        }
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            setState(
                lifecycle,
                initialState(snapshot).copy(
                    phase = GoogleCalendarImportPhase.AUTH_REQUIRED,
                    message = AUTH_REQUIRED,
                ),
            )
            return null
        } ?: run {
            setState(lifecycle, initialState(snapshot))
            return null
        }
        if (
            configuration.configurationId != snapshot.configurationId ||
            configuration.baseUrl.toString() != snapshot.baseUrl
        ) {
            setState(lifecycle, initialState(credentialStore.snapshot()))
            return null
        }
        return BoundGoogleImportConfiguration(
            snapshot = snapshot,
            configuration = configuration,
            configurationId = requireNotNull(snapshot.configurationId),
            apiBaseUrl = requireNotNull(snapshot.baseUrl),
        )
    }

    private fun requireCurrent(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
    ) {
        if (
            !importAllowed() || lifecycleGeneration.get() != lifecycle ||
            !sameBinding(credentialStore.snapshot(), binding.snapshot)
        ) {
            throw StaleGoogleImportOperationException()
        }
    }

    private fun setState(lifecycle: Long, next: GoogleCalendarImportState) {
        val pending = pendingJournalSummary()
        val enriched = next.copy(
            pendingRecoveryCount = pending.count,
            pendingRecoveryAccountIds = pending.accountIds,
        )
        synchronized(presentationMonitor) {
            if (lifecycleGeneration.get() != lifecycle || !operationAllowed()) return
            mutableState.value = enriched
        }
    }

    private fun stateFor(binding: BoundGoogleImportConfiguration): GoogleCalendarImportState {
        val current = mutableState.value
        return if (current.configurationId == binding.configurationId) {
            current
        } else {
            GoogleCalendarImportState(
                phase = GoogleCalendarImportPhase.READY,
                message = READY,
                configurationId = binding.configurationId,
            )
        }
    }

    private fun restoreAfterCancellation(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
    ) {
        if (lifecycleGeneration.get() != lifecycle) return
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.READY,
                message = READY,
                isBusy = false,
                activeAccountId = null,
            ),
        )
    }

    private fun retainRecoveryAfterCancellation(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        accountId: String,
    ) {
        if (lifecycleGeneration.get() != lifecycle) return
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
                message = REFRESH_CANCELLED_RECOVERY_RETAINED,
                isBusy = false,
                activeAccountId = accountId,
                pendingRecoveryCount = pendingCount(),
            ),
        )
    }

    private fun retainRecoveryAfterBindingChange(lifecycle: Long) {
        if (lifecycleGeneration.get() != lifecycle) return
        resetAfterBindingChange(lifecycle)
    }

    private fun resetAfterBindingChange(lifecycle: Long) {
        if (lifecycleGeneration.get() != lifecycle) return
        setState(lifecycle, stateAfterQuarantine(credentialStore.snapshot()))
    }

    private fun stateAfterQuarantine(snapshot: ApiConnectionSnapshot): GoogleCalendarImportState {
        val initial = initialState(snapshot)
        return when (val loaded = journalStore.load(safeNow())) {
            is GoogleCalendarImportJournalLoadResult.Loaded -> if (loaded.journals.isEmpty()) {
                initial
            } else {
                val belongsToCurrentBinding = snapshot.hasBearerToken &&
                    snapshot.configurationId != null && snapshot.baseUrl != null &&
                    loaded.journals.all {
                        it.configurationId == snapshot.configurationId &&
                            it.apiBaseUrl == snapshot.baseUrl
                    }
                initial.copy(
                    phase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
                    message = if (belongsToCurrentBinding) {
                        REFRESH_CHECK_LATER
                    } else {
                        JOURNAL_OTHER_BINDING
                    },
                    pendingRecoveryCount = loaded.journals.size,
                    // A quarantined presentation exposes only the non-identifying count. Exact
                    // account routing is restored by a later authenticated coordinator action.
                    pendingRecoveryAccountIds = emptySet(),
                )
            }

            GoogleCalendarImportJournalLoadResult.Corrupt -> initial.copy(
                phase = GoogleCalendarImportPhase.RECOVERY_REQUIRED,
                message = JOURNAL_UNREADABLE,
                pendingRecoveryCount = 1,
            )
        }
    }

    private fun updateRecoveryFailure(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        message: String,
        pendingCount: Int = pendingCount(),
    ) = updateFailure(
        lifecycle,
        binding,
        GoogleCalendarImportPhase.RECOVERY_REQUIRED,
        message,
        pendingCount,
    )

    private fun updateFailure(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        phase: GoogleCalendarImportPhase,
        message: String,
        pendingCount: Int = pendingCount(),
    ) {
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = phase,
                message = message,
                isBusy = false,
                pendingRecoveryCount = pendingCount,
            ),
        )
    }

    private fun setRefreshPending(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        journal: GoogleCalendarImportJournal,
        message: String,
    ) {
        setState(
            lifecycle,
            stateFor(binding).copy(
                phase = GoogleCalendarImportPhase.CHECKING_COMPLETION,
                message = message,
                isBusy = false,
                activeAccountId = journal.accountId,
                acceptedRefreshGeneration = journal.acceptedRefreshGeneration,
                pendingRecoveryCount = pendingCount(),
            ),
        )
    }

    private fun pendingCount(): Int = when (
        val loaded = journalStore.load(safeNow())
    ) {
        is GoogleCalendarImportJournalLoadResult.Loaded -> loaded.journals.size
        GoogleCalendarImportJournalLoadResult.Corrupt -> 1
    }

    private fun pendingJournalSummary(): PendingJournalSummary = when (
        val loaded = journalStore.load(safeNow())
    ) {
        is GoogleCalendarImportJournalLoadResult.Loaded -> PendingJournalSummary(
            count = loaded.journals.size,
            accountIds = loaded.journals.mapTo(linkedSetOf()) { it.accountId },
        )
        GoogleCalendarImportJournalLoadResult.Corrupt -> PendingJournalSummary(
            count = 1,
            accountIds = emptySet(),
        )
    }

    private fun currentRefreshOutcome(): GoogleCalendarImportOutcome = when (mutableState.value.phase) {
        GoogleCalendarImportPhase.RESPONSE_UNKNOWN -> GoogleCalendarImportOutcome.RESPONSE_UNKNOWN
        GoogleCalendarImportPhase.AUTH_REQUIRED -> GoogleCalendarImportOutcome.AUTH_REQUIRED
        GoogleCalendarImportPhase.ERROR -> GoogleCalendarImportOutcome.FAILED
        else -> GoogleCalendarImportOutcome.RECOVERY_REQUIRED
    }

    private fun bindingFailureOutcome(): GoogleCalendarImportOutcome = when (mutableState.value.phase) {
        GoogleCalendarImportPhase.AUTH_REQUIRED -> GoogleCalendarImportOutcome.AUTH_REQUIRED
        else -> GoogleCalendarImportOutcome.NOT_CONFIGURED
    }

    private fun collectionsBindingFailure(): GoogleImportCollectionsOutcome =
        when (mutableState.value.phase) {
            GoogleCalendarImportPhase.AUTH_REQUIRED -> GoogleImportCollectionsOutcome.AUTH_REQUIRED
            else -> GoogleImportCollectionsOutcome.NOT_CONFIGURED
        }

    private fun configurationBindingFailure(): GoogleImportConfigurationOutcome =
        when (mutableState.value.phase) {
            GoogleCalendarImportPhase.AUTH_REQUIRED -> GoogleImportConfigurationOutcome.AUTH_REQUIRED
            else -> GoogleImportConfigurationOutcome.NOT_CONFIGURED
        }

    private fun configurationFailure(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        error: Exception,
    ): GoogleImportConfigurationOutcome = when (classifyRemoteFailure(error)) {
        RemoteFailureKind.AUTHENTICATION -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.AUTH_REQUIRED, AUTH_REQUIRED)
            GoogleImportConfigurationOutcome.AUTH_REQUIRED
        }

        RemoteFailureKind.CONFLICT -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.ERROR, CONFIGURATION_CONFLICT)
            GoogleImportConfigurationOutcome.CONFLICT
        }

        RemoteFailureKind.OFFLINE, RemoteFailureKind.AMBIGUOUS,
        RemoteFailureKind.RETRYABLE,
        -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.OFFLINE, CONFIGURATION_UNRESOLVED)
            GoogleImportConfigurationOutcome.OFFLINE
        }

        else -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.ERROR, CONFIGURATION_FAILED)
            GoogleImportConfigurationOutcome.FAILED
        }
    }

    private fun collectionsFailure(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        error: Exception,
    ): GoogleImportCollectionsOutcome = when (classifyRemoteFailure(error)) {
        RemoteFailureKind.AUTHENTICATION -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.AUTH_REQUIRED, AUTH_REQUIRED)
            GoogleImportCollectionsOutcome.AUTH_REQUIRED
        }

        RemoteFailureKind.OFFLINE, RemoteFailureKind.AMBIGUOUS,
        RemoteFailureKind.RETRYABLE,
        -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.OFFLINE, COLLECTIONS_OFFLINE)
            GoogleImportCollectionsOutcome.OFFLINE
        }

        else -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.ERROR, COLLECTIONS_FAILED)
            GoogleImportCollectionsOutcome.FAILED
        }
    }

    private fun refreshFailure(
        lifecycle: Long,
        binding: BoundGoogleImportConfiguration,
        error: Exception,
    ): GoogleCalendarImportOutcome = when (classifyRemoteFailure(error)) {
        RemoteFailureKind.AUTHENTICATION -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.AUTH_REQUIRED, AUTH_REQUIRED)
            GoogleCalendarImportOutcome.AUTH_REQUIRED
        }

        RemoteFailureKind.OFFLINE, RemoteFailureKind.AMBIGUOUS,
        RemoteFailureKind.RETRYABLE,
        -> {
            updateFailure(lifecycle, binding, GoogleCalendarImportPhase.RESPONSE_UNKNOWN, REFRESH_RESPONSE_UNKNOWN)
            GoogleCalendarImportOutcome.RESPONSE_UNKNOWN
        }

        else -> {
            updateRecoveryFailure(lifecycle, binding, REFRESH_STATUS_INVALID)
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED
        }
    }

    private fun safeNow(): Long = nowEpochMillis().coerceAtLeast(0L)

    private data class BoundGoogleImportConfiguration(
        val snapshot: ApiConnectionSnapshot,
        val configuration: AuthenticatedApiConfiguration,
        val configurationId: String,
        val apiBaseUrl: String,
    )

    private data class PendingJournalSummary(
        val count: Int,
        val accountIds: Set<String>,
    )

    private class StaleGoogleImportOperationException : IOException(
        "Google import binding changed",
    )

    private class InvalidGoogleImportResponseException : IOException(
        "Google import response was invalid",
    )

    private class ConfigurationNotReconciledException(cause: Throwable) :
        IOException("Google collection configuration was not authoritatively reconciled", cause)

    private enum class RemoteFailureKind {
        AUTHENTICATION,
        CONFLICT,
        OFFLINE,
        AMBIGUOUS,
        RETRYABLE,
        PERMANENT,
        PROTOCOL,
    }

    private fun RemoteFailureKind.isDefinitivePreAcceptanceRejection(): Boolean =
        this == RemoteFailureKind.CONFLICT || this == RemoteFailureKind.PERMANENT

    private fun classifyRemoteFailure(error: Throwable): RemoteFailureKind = when (error) {
        is GoogleCalendarInboundApiException.Authentication -> RemoteFailureKind.AUTHENTICATION
        is GoogleCalendarInboundApiException.Conflict -> RemoteFailureKind.CONFLICT
        is GoogleCalendarInboundApiException.Upstream,
        is GoogleCalendarInboundApiException.Unavailable,
        -> RemoteFailureKind.RETRYABLE
        is GoogleCalendarInboundApiException.InvalidResponse -> RemoteFailureKind.AMBIGUOUS
        is GoogleCalendarInboundApiException.Http -> when {
            error.statusCode >= 500 || error.statusCode in RETRYABLE_HTTP_STATUS_CODES ->
                RemoteFailureKind.RETRYABLE
            error.statusCode in 400..499 -> RemoteFailureKind.PERMANENT
            else -> RemoteFailureKind.AMBIGUOUS
        }
        is GoogleCalendarInboundApiException.NotFound,
        is GoogleCalendarInboundApiException.Validation,
        -> RemoteFailureKind.PERMANENT
        is InvalidGoogleImportResponseException,
        is IllegalArgumentException,
        is DateTimeParseException,
        -> RemoteFailureKind.PROTOCOL
        is ApiBindingChangedException -> RemoteFailureKind.AMBIGUOUS
        is IOException -> RemoteFailureKind.OFFLINE
        else -> RemoteFailureKind.PROTOCOL
    }

    private fun Throwable.requiresAuthoritativeConfigurationRead(): Boolean =
        when (classifyRemoteFailure(this)) {
            RemoteFailureKind.CONFLICT,
            RemoteFailureKind.OFFLINE,
            RemoteFailureKind.AMBIGUOUS,
            RemoteFailureKind.RETRYABLE,
            -> true
            else -> false
        }

    private companion object {
        val RETRYABLE_HTTP_STATUS_CODES = setOf(408, 425, 429)
        const val MAX_COLLECTIONS = 10_000
        const val MAX_COLLECTION_LABEL_LENGTH = 4_096
        const val NOT_CONFIGURED = "Configure the DayWeave API before using Google Calendar."
        const val READY = "Google Calendar import is ready."
        const val AUTH_REQUIRED = "Reconnect the DayWeave API before using Google Calendar."
        const val COLLECTIONS_LOADING = "Loading Google Calendar sources…"
        const val COLLECTIONS_DISCOVERING = "Discovering Google Calendar sources…"
        const val COLLECTIONS_LOADED = "Google Calendar sources are up to date."
        const val COLLECTIONS_DISCOVERED = "Google Calendar source discovery finished."
        const val COLLECTIONS_INVALID = "The selected Google account is invalid."
        const val COLLECTIONS_OFFLINE = "Google Calendar sources are unavailable while offline."
        const val COLLECTIONS_FAILED = "Google Calendar sources could not be loaded safely."
        const val CONFIGURATION_SAVING = "Saving the Google Calendar import policy…"
        const val CONFIGURATION_SAVED = "The Google Calendar import policy was saved."
        const val CONFIGURATION_RECONCILED =
            "The Google Calendar import policy was verified from authoritative state."
        const val CONFIGURATION_INVALID = "The Google Calendar source request is invalid."
        const val CONFIGURATION_CONFLICT =
            "The Google Calendar source changed elsewhere; reload before trying again."
        const val CONFIGURATION_UNRESOLVED =
            "The Google Calendar change is unconfirmed; authoritative state will be checked again."
        const val CONFIGURATION_FAILED = "The Google Calendar import policy was not changed."
        const val CONFIGURATION_IMPORT_PENDING =
            "Finish the saved Google import before changing Calendar source settings."
        const val REFRESH_INVALID = "The selected Google account is invalid."
        const val REFRESH_PREPARING = "Saving a recoverable Google import request…"
        const val REFRESH_REQUESTING = "Requesting a Google Calendar import…"
        const val REFRESH_RESPONSE_UNKNOWN =
            "The response was interrupted; the exact request is saved for safe replay."
        const val REFRESH_REJECTED =
            "The server rejected the import request; retrying will use a new request."
        const val REFRESH_REJECTED_RECOVERY_RETAINED =
            "The import did not complete; its recovery record was retained."
        const val REFRESH_ACCEPTED = "Google accepted the import request."
        const val REFRESH_CHECKING = "Checking authoritative Google import status…"
        const val REFRESH_CHECK_LATER =
            "The Google import is still pending; its completion record remains saved."
        const val REFRESH_SERVER_BACKOFF =
            "Google import is waiting for the server's bounded retry window."
        const val REFRESH_AUTH_REQUIRED =
            "Google authorization must be renewed before import can continue."
        const val REFRESH_OFFLINE = "The import remains saved while this device is offline."
        const val REFRESH_FAILED = "Google import needs attention; recovery remains saved."
        const val TERMINAL_RESTART_PREPARING =
            "Saving a new request for the terminal Google import…"
        const val TERMINAL_RESTART_NOT_SAVED =
            "The terminal import remains saved because its retry could not be prepared safely."
        const val REFRESH_STATUS_INVALID =
            "Authoritative import status could not be verified; recovery remains saved."
        const val CANONICAL_PERSISTING =
            "Refreshing, composing, publishing, and saving the canonical schedule…"
        const val CANONICAL_NOT_PERSISTED =
            "The canonical schedule is not durably complete; import recovery remains saved."
        const val COMPLETION_NOT_CLEARED =
            "The schedule is durable, but the recovery marker still needs cleanup."
        const val REFRESH_COMPLETED = "Google import and canonical schedule refresh completed."
        const val REFRESH_CANCELLED_RECOVERY_RETAINED =
            "Import work stopped safely; its exact recovery record remains saved."
        const val JOURNAL_UNREADABLE =
            "The saved Google import recovery is unreadable and must be repaired explicitly."
        const val JOURNAL_OTHER_BINDING =
            "A saved Google import belongs to another API credential generation."
        const val JOURNAL_NOT_SAVED =
            "The recoverable import request could not be saved, so nothing was sent."
        const val JOURNAL_ABANDONMENT_FAILED =
            "The saved Google import recovery could not be abandoned; credentials were retained."
        const val ACCEPTANCE_NOT_SAVED =
            "Google may have accepted the import; the exact request remains saved for replay."
        const val NO_PENDING_REFRESH = "No saved Google import is waiting for this account."

        fun initialState(snapshot: ApiConnectionSnapshot): GoogleCalendarImportState =
            if (snapshot.hasBearerToken && snapshot.configurationId != null) {
                GoogleCalendarImportState(
                    phase = GoogleCalendarImportPhase.READY,
                    message = READY,
                    configurationId = snapshot.configurationId,
                )
            } else {
                GoogleCalendarImportState(
                    phase = GoogleCalendarImportPhase.NOT_CONFIGURED,
                    message = NOT_CONFIGURED,
                    configurationId = snapshot.configurationId,
                )
            }

        fun sameBinding(left: ApiConnectionSnapshot, right: ApiConnectionSnapshot): Boolean =
            left.baseUrl == right.baseUrl &&
                left.hasBearerToken == right.hasBearerToken &&
                left.configurationId == right.configurationId
    }
}

private fun RemoteGoogleSyncCollection.matches(request: ConfigureGoogleCollectionRequest): Boolean {
    val expectedSelected = request.role != GoogleInboundCollectionRole.OFF
    val expectedVisible =
        request.role != GoogleInboundCollectionRole.OFF && request.visible
    val expectedRole = when (request.role) {
        GoogleInboundCollectionRole.OFF,
        GoogleInboundCollectionRole.READ_ONLY,
        -> RemoteGoogleSyncRole.READ_ONLY
        GoogleInboundCollectionRole.BLOCKING -> RemoteGoogleSyncRole.BLOCKING
    }
    return kind == request.kind &&
        selected == expectedSelected &&
        visible == expectedVisible &&
        syncRole == expectedRole &&
        calendarPolicy == request.calendarPolicy
}

private fun RemoteGoogleSyncCollection.toState(): GoogleImportCollectionState =
    GoogleImportCollectionState(
        id = id,
        accountId = accountId,
        displayName = displayName,
        kind = kind,
        providerDeleted = providerDeleted,
        selected = selected,
        visible = visible,
        syncRole = syncRole,
        calendarPolicy = calendarPolicy,
        revision = revision,
        lastImportAt = lastImportAt,
        providerAccessRole = providerAccessRole,
    )

private fun RemoteGoogleSyncRunStatus.toState(): GoogleImportRunState = GoogleImportRunState(
    state = state,
    refreshGeneration = refreshGeneration,
    claimedRefreshGeneration = claimedRefreshGeneration,
    completedRefreshGeneration = completedRefreshGeneration,
    nextAttemptAt = Instant.parse(nextAttemptAt),
    importedCount = importedCount,
    updatedCount = updatedCount,
    deletedCount = deletedCount,
    conflictCount = conflictCount,
    rejectedCount = rejectedCount,
)

private fun String.isCanonicalGoogleUuid(): Boolean = runCatching {
    val parsed = UUID.fromString(this)
    parsed.toString() == this && parsed != UUID(0, 0)
}.getOrDefault(false)
