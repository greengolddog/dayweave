package com.greengolddog.dayweave.network

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

enum class AccountRecoveryPhase {
    LOCKED,
    NOT_AVAILABLE,
    LOADING,
    READY,
    PENDING,
    DISCLOSURE_READY,
    OFFLINE,
    AUTH_REQUIRED,
    REPAIR_REQUIRED,
    ERROR,
}

data class AccountRecoveryState(
    val phase: AccountRecoveryPhase,
    val currentCodeId: String? = null,
    val currentCodeCreatedAt: String? = null,
    val currentCodeRevision: Long? = null,
    val message: String,
    val isBusy: Boolean = false,
    val canIssueOrRotate: Boolean = false,
    val disclosureReady: Boolean = false,
    val retryAvailable: Boolean = false,
    val discardAvailable: Boolean = false,
    val repairRequired: Boolean = false,
    val deviceAuthorizationSuppressed: Boolean = false,
)

class AccountRecoveryIssuanceConfirmation internal constructor(
    internal val generation: Long,
    internal val binding: ApiConnectionSnapshot,
    internal val currentCodeId: String?,
    internal val currentCodeRevision: Long?,
) {
    override fun toString(): String = "AccountRecoveryIssuanceConfirmation(<redacted>)"
}

class AccountRecoveryDisclosure internal constructor(
    internal val generation: Long,
    internal val journalId: String,
    internal val journalCode: String,
    val source: String,
) {
    val code: String get() = journalCode

    override fun toString(): String = "AccountRecoveryDisclosure(<redacted>)"
}

class AccountRecoveryJournalDiscardConfirmation internal constructor(
    internal val generation: Long,
    internal val expected: StoredDeviceAuthEnvelope,
    val repairsUnreadableState: Boolean,
) {
    override fun toString(): String = "AccountRecoveryJournalDiscardConfirmation(<redacted>)"
}

/**
 * Owns recovery-code metadata presentation and durable mutation replay. Plaintext is returned only
 * through an explicitly requested disclosure and never enters [state].
 */
internal class AccountRecoveryManager(
    private val store: DeviceAuthEnvelopeStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: AccountRecoveryTransport,
    private val bindingOperationGate: ApiBindingOperationGate,
    private val bindingFence: DeviceAuthBindingFence,
    private val deviceLabel: String,
    private val clientVersion: String,
    private val generator: DeviceCredentialGenerator = SecureDeviceCredentialGenerator,
    private val now: () -> Instant = Instant::now,
    private val operationAllowed: () -> Boolean = { true },
    private val allowCleartextLoopbackForTests: Boolean = false,
) {
    private val operationMutex = Mutex()
    private val presentationLock = Any()
    private var presentationGeneration = 1L
    private val activeJobs = mutableSetOf<Job>()
    private val mutableState = MutableStateFlow(initialState())
    val state: StateFlow<AccountRecoveryState> = mutableState.asStateFlow()

    init {
        requireValidDeviceIdentity(deviceLabel, clientVersion)
    }

    fun quarantineForPrivacyBoundary() {
        val jobs = synchronized(presentationLock) {
            presentationGeneration = nextGeneration(presentationGeneration)
            mutableState.value = lockedState()
            activeJobs.toList().also { activeJobs.clear() }
        }
        jobs.forEach { it.cancel(CancellationException("Account recovery crossed a privacy boundary")) }
    }

    fun hasDurableBlocker(): Boolean = store.read().accountRecoveryJournal != null

    suspend fun refresh() {
        withPresentationOperation { generation ->
            operationMutex.withLock {
                val envelope = store.read()
                if (publishJournal(envelope, generation)) return@withLock
                val binding = credentialStore.snapshot()
                val configuration = exactConfiguration(binding)
                if (configuration == null) {
                    publish(
                        generation,
                        AccountRecoveryState(
                            phase = AccountRecoveryPhase.NOT_AVAILABLE,
                            message = "Connect a full-owner device session to manage recovery.",
                        ),
                    )
                    return@withLock
                }
                publish(
                    generation,
                    AccountRecoveryState(
                        phase = AccountRecoveryPhase.LOADING,
                        message = "Checking account recovery…",
                        isBusy = true,
                    ),
                )
                try {
                    val response = configuration.withBindingOperation {
                        transport.current(configuration)
                    }
                    if (!bindingStillCurrent(binding)) {
                        publishInitial(generation)
                        return@withLock
                    }
                    publish(
                        generation,
                        readyState(response.recoveryCode, binding),
                        advance = true,
                    )
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    publish(generation, failureState(error))
                }
            }
        }
    }

    fun issuanceConfirmation(): AccountRecoveryIssuanceConfirmation? {
        val binding = credentialStore.snapshot()
        return synchronized(presentationLock) {
            val current = mutableState.value
            if (
                !operationAllowed() || current.phase != AccountRecoveryPhase.READY ||
                !current.canIssueOrRotate || !binding.hasBearerToken ||
                binding.configurationId == null || binding.clientInstanceId == null ||
                store.read().accountRecoveryJournal != null
            ) return@synchronized null
            AccountRecoveryIssuanceConfirmation(
                generation = presentationGeneration,
                binding = binding,
                currentCodeId = current.currentCodeId,
                currentCodeRevision = current.currentCodeRevision,
            )
        }
    }

    suspend fun issueOrRotate(
        confirmation: AccountRecoveryIssuanceConfirmation,
    ): DeviceAuthActionResult = withPresentationOperation(confirmation.generation) { generation ->
        operationMutex.withLock {
            val envelope = store.read()
            val binding = credentialStore.snapshot()
            if (
                binding != confirmation.binding || !binding.hasBearerToken ||
                envelope.accountRecoveryJournal != null ||
                !stateCanManageRecovery(envelope.state, binding)
            ) return@withLock DeviceAuthActionResult.STALE_STATE
            val current = mutableState.value
            if (
                current.phase != AccountRecoveryPhase.READY ||
                current.currentCodeId != confirmation.currentCodeId ||
                current.currentCodeRevision != confirmation.currentCodeRevision
            ) return@withLock DeviceAuthActionResult.STALE_STATE
            val candidate = generateRecoveryCode(envelope.state)
                ?: return@withLock DeviceAuthActionResult.STORAGE_FAILURE
            val pending = StoredAccountRecoveryJournal.IssuancePending(
                baseUrl = requireNotNull(binding.baseUrl),
                configurationId = requireNotNull(binding.configurationId),
                clientInstanceId = requireNotNull(binding.clientInstanceId),
                candidateId = candidate.first,
                candidateCode = DeviceAuthSecret(candidate.second),
                replacesId = confirmation.currentCodeId,
                replacesRevision = confirmation.currentCodeRevision,
                preparedAt = now().toString(),
            )
            if (!transition(envelope, envelope.state, pending)) {
                return@withLock DeviceAuthActionResult.STALE_STATE
            }
            publish(
                generation,
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.PENDING,
                    message = "Recovery-code issuance is journaled for exact retry.",
                    isBusy = true,
                    retryAvailable = true,
                ),
            )
            completeIssuance(generation)
        }
    } ?: DeviceAuthActionResult.NOT_ALLOWED

    suspend fun consume(
        baseUrl: String,
        recoveryCode: String,
        confirmed: Boolean,
    ): DeviceAuthActionResult {
        if (!confirmed) return DeviceAuthActionResult.NOT_ALLOWED
        return withPresentationOperation { generation ->
            operationMutex.withLock {
                val envelope = store.read()
                if (
                    envelope.state is StoredDeviceAuthState.Incompatible ||
                    envelope.accountRecoveryJournal != null
                ) return@withLock DeviceAuthActionResult.NOT_ALLOWED
                val normalized = try {
                    normalizeBaseUrlForDeviceAuth(
                        baseUrl.trim(),
                        allowCleartextLoopbackForTests,
                    ).toString()
                } catch (_: IllegalArgumentException) {
                    publish(generation, errorState("Enter a valid HTTPS DayWeave API endpoint."))
                    return@withLock DeviceAuthActionResult.SERVER_REJECTED
                }
                val enteredCode = recoveryCode.trim()
                try {
                    validateExactDeviceToken(enteredCode, ACCOUNT_RECOVERY_TOKEN_PREFIX)
                } catch (_: IllegalArgumentException) {
                    publish(generation, errorState("Enter an exact dw_rc1_ recovery code."))
                    return@withLock DeviceAuthActionResult.SERVER_REJECTED
                }
                val tuple = generateConsumptionTuple(enteredCode, envelope)
                    ?: return@withLock DeviceAuthActionResult.STORAGE_FAILURE
                val pending = StoredAccountRecoveryJournal.ConsumptionPending(
                    baseUrl = normalized,
                    previousBaseUrl = envelope.state.baseUrl,
                    previousBindingId = previousBinding(envelope.state),
                    clientInstanceId = tuple.clientInstanceId,
                    sessionId = tuple.sessionId,
                    deviceLabel = deviceLabel,
                    clientVersion = clientVersion,
                    preparedAt = now().toString(),
                    recoveryCode = DeviceAuthSecret(enteredCode),
                    accessToken = DeviceAuthSecret(tuple.accessToken),
                    refreshToken = DeviceAuthSecret(tuple.refreshToken),
                    successorId = tuple.successorId,
                    successorCode = DeviceAuthSecret(tuple.successorCode),
                )
                try {
                    bindingOperationGate.invalidateBeforeQuarantine {
                        if (store.read() != envelope) {
                            return@invalidateBeforeQuarantine DeviceAuthActionResult.STALE_STATE
                        }
                        if (
                            !bindingFence.beforeAccountRecoveryRequest(
                                envelope.state.baseUrl,
                                previousBinding(envelope.state),
                                normalized,
                            )
                        ) {
                            publish(
                                generation,
                                errorState(
                                    "Finish the saved Planner or Google operation before " +
                                        "starting account recovery.",
                                ),
                            )
                            return@invalidateBeforeQuarantine DeviceAuthActionResult.NOT_ALLOWED
                        }
                        if (
                            store.read() != envelope ||
                            !transition(envelope, envelope.state, pending)
                        ) {
                            return@invalidateBeforeQuarantine DeviceAuthActionResult.STALE_STATE
                        }
                        publish(
                            generation,
                            AccountRecoveryState(
                                phase = AccountRecoveryPhase.PENDING,
                                message = "Recovery is journaled. Existing API credentials are paused.",
                                isBusy = true,
                                retryAvailable = true,
                                deviceAuthorizationSuppressed = true,
                            ),
                        )
                        completeConsumption(generation)
                    }
                } catch (_: IllegalStateException) {
                    DeviceAuthActionResult.STORAGE_FAILURE
                }
            }
        } ?: DeviceAuthActionResult.NOT_ALLOWED
    }

    suspend fun retryPending(): DeviceAuthActionResult = withPresentationOperation { generation ->
        operationMutex.withLock {
            when (store.read().accountRecoveryJournal) {
                is StoredAccountRecoveryJournal.IssuancePending -> completeIssuance(generation)
                is StoredAccountRecoveryJournal.ConsumptionPending -> try {
                    bindingOperationGate.invalidateBeforeQuarantine {
                        completeConsumption(generation)
                    }
                } catch (_: IllegalStateException) {
                    DeviceAuthActionResult.STORAGE_FAILURE
                }
                is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation -> try {
                    bindingOperationGate.invalidateBeforeQuarantine {
                        installCommittedConsumption(generation)
                    }
                } catch (_: IllegalStateException) {
                    DeviceAuthActionResult.STORAGE_FAILURE
                }
                else -> DeviceAuthActionResult.NOT_ALLOWED
            }
        }
    } ?: DeviceAuthActionResult.NOT_ALLOWED

    fun journalDiscardConfirmation(): AccountRecoveryJournalDiscardConfirmation? {
        return synchronized(presentationLock) {
            if (!operationAllowed()) return@synchronized null
            val expected = store.read()
            val journal = expected.accountRecoveryJournal ?: return@synchronized null
            if (
                journal is StoredAccountRecoveryJournal.DisclosurePending ||
                journal is
                StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation
            ) {
                return@synchronized null
            }
            if (!mutableState.value.discardAvailable) return@synchronized null
            AccountRecoveryJournalDiscardConfirmation(
                generation = presentationGeneration,
                expected = expected,
                repairsUnreadableState =
                    journal is StoredAccountRecoveryJournal.RepairRequired,
            )
        }
    }

    suspend fun discardJournal(
        confirmation: AccountRecoveryJournalDiscardConfirmation,
    ): Boolean = withPresentationOperation(confirmation.generation) { generation ->
        operationMutex.withLock {
            val current = store.read()
            if (current != confirmation.expected) return@withLock false
            val journal = current.accountRecoveryJournal ?: return@withLock false
            if (
                journal is StoredAccountRecoveryJournal.DisclosurePending ||
                journal is
                StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation
            ) return@withLock false
            if (!transition(current, current.state, null)) return@withLock false
            publish(
                generation,
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.NOT_AVAILABLE,
                    message = if (confirmation.repairsUnreadableState) {
                        "Unreadable recovery state removed. Refresh before starting again."
                    } else {
                        "Saved recovery request discarded. Refresh before starting again."
                    },
                ),
                advance = true,
            )
            true
        }
    } ?: false

    fun disclosure(): AccountRecoveryDisclosure? = synchronized(presentationLock) {
        if (!operationAllowed()) return@synchronized null
        val journal = store.read().accountRecoveryJournal as?
            StoredAccountRecoveryJournal.DisclosurePending ?: return@synchronized null
        if (!mutableState.value.disclosureReady) return@synchronized null
        AccountRecoveryDisclosure(
            generation = presentationGeneration,
            journalId = journal.id,
            journalCode = journal.code.value,
            source = journal.source,
        )
    }

    suspend fun acknowledge(disclosure: AccountRecoveryDisclosure): Boolean =
        withPresentationOperation(disclosure.generation) { generation ->
            operationMutex.withLock {
                val envelope = store.read()
                val journal = envelope.accountRecoveryJournal as?
                    StoredAccountRecoveryJournal.DisclosurePending ?: return@withLock false
                if (journal.id != disclosure.journalId || journal.code.value != disclosure.journalCode) {
                    return@withLock false
                }
                if (!transition(envelope, envelope.state, null)) return@withLock false
                publish(
                    generation,
                    AccountRecoveryState(
                        phase = AccountRecoveryPhase.READY,
                        currentCodeId = journal.id,
                        currentCodeCreatedAt = journal.createdAt,
                        currentCodeRevision = journal.revision,
                        message = "Recovery code saved. Rotate it if disclosure is suspected.",
                        canIssueOrRotate = stateCanManageRecovery(
                            store.read().state,
                            credentialStore.snapshot(),
                        ),
                    ),
                    advance = true,
                )
                true
            }
        } ?: false

    private suspend fun completeIssuance(generation: Long): DeviceAuthActionResult {
        val expected = store.read()
        val pending = expected.accountRecoveryJournal as?
            StoredAccountRecoveryJournal.IssuancePending
            ?: return DeviceAuthActionResult.STALE_STATE
        val configuration = credentialStore.authenticatedConfiguration()
        if (
            configuration == null || configuration.configurationId != pending.configurationId ||
            configuration.baseUrl.toString() != pending.baseUrl
        ) {
            publish(generation, authRequiredState("Reconnect this device to retry issuance."))
            return DeviceAuthActionResult.AUTH_REQUIRED
        }
        val result = try {
            configuration.withBindingOperation {
                transport.issue(
                    configuration,
                    CreateAccountRecoveryCodeRequest(
                        id = pending.candidateId,
                        recoveryCode = pending.candidateCode.value,
                        replacesRecoveryCodeId = pending.replacesId,
                        replacesRecoveryCodeRevision = pending.replacesRevision,
                    ),
                    Instant.parse(pending.preparedAt),
                )
            }
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            return handlePendingFailure(expected, generation, error)
        }
        val disclosure = StoredAccountRecoveryJournal.DisclosurePending(
            baseUrl = pending.baseUrl,
            id = result.recoveryCode.id,
            code = pending.candidateCode,
            createdAt = result.recoveryCode.createdAt,
            revision = result.recoveryCode.revision,
            source = "issued",
        )
        if (!transition(expected, expected.state, disclosure)) {
            publishInitial(generation)
            return DeviceAuthActionResult.STALE_STATE
        }
        publish(generation, disclosureState(disclosure), advance = true)
        return DeviceAuthActionResult.SUCCESS
    }

    private suspend fun completeConsumption(generation: Long): DeviceAuthActionResult {
        val expected = store.read()
        val pending = expected.accountRecoveryJournal as?
            StoredAccountRecoveryJournal.ConsumptionPending
            ?: return DeviceAuthActionResult.STALE_STATE
        val result = try {
            transport.consume(
                pending.baseUrl,
                pending.recoveryCode.value,
                ConsumeAccountRecoveryCodeRequest(
                    sessionId = pending.sessionId,
                    accessToken = pending.accessToken.value,
                    refreshToken = pending.refreshToken.value,
                    clientInstanceId = pending.clientInstanceId,
                    deviceLabel = pending.deviceLabel,
                    clientVersion = pending.clientVersion,
                    successorRecoveryCodeId = pending.successorId,
                    successorRecoveryCode = pending.successorCode.value,
                ),
                Instant.parse(pending.preparedAt),
            )
        } catch (error: CancellationException) {
            throw error
        } catch (error: Exception) {
            return handlePendingFailure(expected, generation, error)
        }
        val committed = StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation(
            baseUrl = pending.baseUrl,
            previousBaseUrl = pending.previousBaseUrl,
            previousBindingId = pending.previousBindingId,
            clientInstanceId = pending.clientInstanceId,
            session = result.session,
            accessToken = pending.accessToken,
            refreshToken = pending.refreshToken,
            successorId = result.successorRecoveryCode.id,
            successorCode = pending.successorCode,
            successorCreatedAt = result.successorRecoveryCode.createdAt,
            successorRevision = result.successorRecoveryCode.revision,
        )
        if (!transition(expected, expected.state, committed)) {
            publishInitial(generation)
            return DeviceAuthActionResult.STALE_STATE
        }
        return installCommittedConsumption(generation)
    }

    private suspend fun installCommittedConsumption(
        generation: Long,
    ): DeviceAuthActionResult {
        val expected = store.read()
        val committed = expected.accountRecoveryJournal as?
            StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation
            ?: return DeviceAuthActionResult.STALE_STATE
        val active = StoredDeviceAuthState.Active(
            baseUrl = committed.baseUrl,
            clientInstanceId = committed.clientInstanceId,
            session = committed.session,
            accessToken = committed.accessToken,
            refreshToken = committed.refreshToken,
        )
        val disclosure = StoredAccountRecoveryJournal.DisclosurePending(
            baseUrl = committed.baseUrl,
            id = committed.successorId,
            code = committed.successorCode,
            createdAt = committed.successorCreatedAt,
            revision = committed.successorRevision,
            source = "successor",
        )
        val installed = try {
            store.read() == expected &&
                bindingFence.beforeAccountRecovery(
                    committed.previousBaseUrl,
                    committed.previousBindingId,
                    committed.baseUrl,
                    committed.session.id,
                ) &&
                store.read() == expected && transition(expected, active, disclosure)
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            false
        }
        if (!installed) {
            publish(
                generation,
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.PENDING,
                    message = "Recovery is committed. Local cache quarantine and credential " +
                        "installation must finish before DayWeave can continue.",
                    retryAvailable = true,
                    deviceAuthorizationSuppressed = true,
                ),
            )
            return DeviceAuthActionResult.CACHE_FENCE_BLOCKED
        }
        publish(generation, disclosureState(disclosure), advance = true)
        return DeviceAuthActionResult.SUCCESS
    }

    private fun handlePendingFailure(
        expected: StoredDeviceAuthEnvelope,
        generation: Long,
        error: Exception,
    ): DeviceAuthActionResult {
        val journal = expected.accountRecoveryJournal
        val safelyRejectedBeforeCommit = when (journal) {
            is StoredAccountRecoveryJournal.IssuancePending ->
                error is DeviceAuthApiException.Forbidden ||
                    error is DeviceAuthApiException.Conflict ||
                    error is DeviceAuthApiException.Validation
            is StoredAccountRecoveryJournal.ConsumptionPending ->
                error is DeviceAuthApiException.Validation
            else -> false
        }
        if (safelyRejectedBeforeCommit) {
            if (!transition(expected, expected.state, null)) {
                publishInitial(generation)
                return DeviceAuthActionResult.STALE_STATE
            }
        }
        return when (error) {
            is DeviceAuthenticationChangedException, is ApiBindingChangedException -> {
                publishInitial(generation)
                DeviceAuthActionResult.STALE_STATE
            }
            is DeviceAuthenticationRequiredException,
            is DeviceAuthApiException.Authentication,
            -> {
                publish(
                    generation,
                    authRequiredState(
                        "Recovery authentication was rejected. The exact saved request was " +
                            "retained because the server outcome may have changed.",
                        retryAvailable = true,
                        discardAvailable = true,
                    ).copy(deviceAuthorizationSuppressed = journal.blocksApiBoundWork()),
                )
                DeviceAuthActionResult.AUTH_REQUIRED
            }
            is DeviceAuthApiException.Forbidden -> {
                if (safelyRejectedBeforeCommit) {
                    publish(generation, errorState("This session lacks full owner recovery authority."))
                } else {
                    publish(
                        generation,
                        retainedPendingErrorState(
                            "Recovery was rejected.",
                            journal.blocksApiBoundWork(),
                        ),
                    )
                }
                DeviceAuthActionResult.NOT_ALLOWED
            }
            is DeviceAuthApiException.Conflict, is DeviceAuthApiException.Validation -> {
                if (safelyRejectedBeforeCommit) {
                    publish(generation, errorState("Recovery state changed; refresh and try again."))
                } else {
                    publish(
                        generation,
                        retainedPendingErrorState(
                            "Recovery state changed.",
                            journal.blocksApiBoundWork(),
                        ),
                    )
                }
                DeviceAuthActionResult.SERVER_REJECTED
            }
            is DeviceAuthApiException.InvalidResponse -> {
                publish(
                    generation,
                    pendingErrorState(
                        "The recovery response was invalid; exact retry is available.",
                        journal.blocksApiBoundWork(),
                    ),
                )
                DeviceAuthActionResult.SERVER_REJECTED
            }
            is DeviceAuthApiException.Unavailable, is IOException -> {
                publish(generation, pendingOfflineState(journal.blocksApiBoundWork()))
                DeviceAuthActionResult.PENDING_RETRY
            }
            else -> {
                publish(
                    generation,
                    pendingErrorState(
                        "Recovery is pending exact retry.",
                        journal.blocksApiBoundWork(),
                    ),
                )
                DeviceAuthActionResult.PENDING_RETRY
            }
        }
    }

    private fun publishJournal(envelope: StoredDeviceAuthEnvelope, generation: Long): Boolean {
        val state = when (val journal = envelope.accountRecoveryJournal) {
            null -> return false
            is StoredAccountRecoveryJournal.RepairRequired -> AccountRecoveryState(
                phase = AccountRecoveryPhase.REPAIR_REQUIRED,
                message = "Saved recovery state is not readable by this build. Confirm removal " +
                    "of recovery state only, or update DayWeave.",
                discardAvailable = true,
                repairRequired = true,
                deviceAuthorizationSuppressed = true,
            )
            is StoredAccountRecoveryJournal.IssuancePending -> AccountRecoveryState(
                phase = AccountRecoveryPhase.PENDING,
                message = "Recovery-code issuance is journaled for exact retry.",
                retryAvailable = true,
                discardAvailable = true,
            )
            is StoredAccountRecoveryJournal.ConsumptionPending -> AccountRecoveryState(
                phase = AccountRecoveryPhase.PENDING,
                message = "Account recovery is journaled; old API credentials remain paused.",
                retryAvailable = true,
                discardAvailable = true,
                deviceAuthorizationSuppressed = true,
            )
            is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation ->
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.PENDING,
                    message = "Recovery is committed. Finish local cache quarantine and install " +
                        "the recovered credentials.",
                    retryAvailable = true,
                    deviceAuthorizationSuppressed = true,
                )
            is StoredAccountRecoveryJournal.DisclosurePending -> disclosureState(journal)
        }
        publish(generation, state)
        return true
    }

    private fun readyState(
        code: AccountRecoveryCodeContract?,
        binding: ApiConnectionSnapshot,
    ) = AccountRecoveryState(
        phase = AccountRecoveryPhase.READY,
        currentCodeId = code?.id,
        currentCodeCreatedAt = code?.createdAt,
        currentCodeRevision = code?.revision,
        message = if (code == null) {
            "No account recovery code is active."
        } else {
            "One recovery code is active. Rotating immediately retires the old code."
        },
        canIssueOrRotate = stateCanManageRecovery(store.read().state, binding),
    )

    private fun disclosureState(journal: StoredAccountRecoveryJournal.DisclosurePending) =
        AccountRecoveryState(
            phase = AccountRecoveryPhase.DISCLOSURE_READY,
            currentCodeId = journal.id,
            currentCodeCreatedAt = journal.createdAt,
            currentCodeRevision = journal.revision,
            message = if (journal.source == "successor") {
                "Recovery succeeded. Save the successor code before leaving."
            } else {
                "A new recovery code is ready. Save it before leaving."
            },
            disclosureReady = true,
        )

    private fun stateCanManageRecovery(
        state: StoredDeviceAuthState,
        binding: ApiConnectionSnapshot,
    ): Boolean {
        val session = when (state) {
            is StoredDeviceAuthState.Active -> state.session
            is StoredDeviceAuthState.RefreshPending -> state.session
            else -> return false
        }
        return binding.hasBearerToken && binding.configurationId == session.id &&
            binding.clientInstanceId == session.clientInstanceId &&
            session.clientKind == "android" && session.scopes == ANDROID_DEVICE_AUTH_SCOPES
    }

    private fun exactConfiguration(
        binding: ApiConnectionSnapshot,
    ): AuthenticatedApiConfiguration? {
        if (
            !binding.hasBearerToken || binding.baseUrl == null ||
            binding.configurationId == null || binding.clientInstanceId == null
        ) return null
        val configuration = credentialStore.authenticatedConfiguration() ?: return null
        return configuration.takeIf {
            it.configurationId == binding.configurationId &&
                it.baseUrl.toString() == binding.baseUrl
        }
    }

    private fun bindingStillCurrent(binding: ApiConnectionSnapshot): Boolean =
        credentialStore.snapshot() == binding && store.read().accountRecoveryJournal == null

    private fun generateRecoveryCode(state: StoredDeviceAuthState): Pair<String, String>? =
        runCatching {
            repeat(MAX_GENERATION_ATTEMPTS) {
                val id = generator.sessionId()
                requireRecoveryNonNilUuid(id)
                val code = generator.token(ACCOUNT_RECOVERY_TOKEN_PREFIX)
                validateExactDeviceToken(code, ACCOUNT_RECOVERY_TOKEN_PREFIX)
                val existing = when (state) {
                    is StoredDeviceAuthState.Active ->
                        listOf(state.accessToken.value, state.refreshToken.value)
                    is StoredDeviceAuthState.RefreshPending -> listOf(
                        state.currentAccessToken.value,
                        state.currentRefreshToken.value,
                        state.nextAccessToken.value,
                        state.nextRefreshToken.value,
                    )
                    else -> emptyList()
                }
                if (runCatching {
                        requireDistinctCredentialMaterials(*(existing + code).toTypedArray())
                    }.isSuccess
                ) return@runCatching id to code
            }
            null
        }.getOrNull()

    private fun generateConsumptionTuple(
        recoveryCode: String,
        envelope: StoredDeviceAuthEnvelope,
    ): ConsumptionTuple? = runCatching {
        repeat(MAX_GENERATION_ATTEMPTS) {
            val sessionId = generator.sessionId()
            val successorId = generator.sessionId()
            val access = generator.token(DEVICE_ACCESS_TOKEN_PREFIX)
            val refresh = generator.token(DEVICE_REFRESH_TOKEN_PREFIX)
            val successor = generator.token(ACCOUNT_RECOVERY_TOKEN_PREFIX)
            if (
                runCatching {
                    requireRecoveryNonNilUuid(sessionId)
                    requireRecoveryNonNilUuid(successorId)
                }.isFailure ||
                sessionId == successorId || sessionId == previousBinding(envelope.state)
            ) {
                return@repeat
            }
            validateExactDeviceToken(access, DEVICE_ACCESS_TOKEN_PREFIX)
            validateExactDeviceToken(refresh, DEVICE_REFRESH_TOKEN_PREFIX)
            validateExactDeviceToken(successor, ACCOUNT_RECOVERY_TOKEN_PREFIX)
            if (runCatching {
                    requireDistinctCredentialMaterials(recoveryCode, access, refresh, successor)
                }.isSuccess
            ) {
                return@runCatching ConsumptionTuple(
                    clientInstanceId = envelope.state.clientInstanceId
                        ?: generator.sessionId().also(::requireRecoveryNonNilUuid),
                    sessionId = sessionId,
                    accessToken = access,
                    refreshToken = refresh,
                    successorId = successorId,
                    successorCode = successor,
                )
            }
        }
        null
    }.getOrNull()

    private fun previousBinding(state: StoredDeviceAuthState): String? = when (state) {
        is StoredDeviceAuthState.Reauth -> state.previousSessionId
        is StoredDeviceAuthState.EnrollmentCreationPending ->
            state.previousBindingId ?: state.enrollmentId
        is StoredDeviceAuthState.EnrollmentPending -> state.previousBindingId ?: state.sessionId
        else -> state.bindingId()
    }

    private fun transition(
        expected: StoredDeviceAuthEnvelope,
        nextState: StoredDeviceAuthState,
        nextJournal: StoredAccountRecoveryJournal?,
    ): Boolean {
        return try {
            if (!store.compareAndSet(expected, nextState, nextJournal)) return false
            val readback = store.read()
            readback.revision == Math.addExact(expected.revision, 1) &&
                readback.state == nextState && readback.accountRecoveryJournal == nextJournal
        } catch (_: RuntimeException) {
            false
        }
    }

    private fun failureState(error: Exception): AccountRecoveryState = when (error) {
        is DeviceAuthenticationRequiredException, is DeviceAuthApiException.Authentication ->
            authRequiredState("Reconnect this device to manage account recovery.")
        is DeviceAuthApiException.Forbidden ->
            errorState("This session can view recovery metadata but lacks full owner authority.")
        is IOException -> AccountRecoveryState(
            phase = AccountRecoveryPhase.OFFLINE,
            message = "Offline · account recovery metadata was not retained.",
        )
        else -> errorState("The account recovery response was invalid.")
    }

    private fun pendingOfflineState(deviceAuthorizationSuppressed: Boolean = false) =
        AccountRecoveryState(
        phase = AccountRecoveryPhase.OFFLINE,
        message = "Offline · the exact recovery request remains journaled for retry.",
        retryAvailable = true,
        discardAvailable = true,
        deviceAuthorizationSuppressed = deviceAuthorizationSuppressed,
    )

    private fun pendingErrorState(
        message: String,
        deviceAuthorizationSuppressed: Boolean = false,
    ) = AccountRecoveryState(
        phase = AccountRecoveryPhase.PENDING,
        message = message,
        retryAvailable = true,
        discardAvailable = true,
        deviceAuthorizationSuppressed = deviceAuthorizationSuppressed,
    )

    private fun retainedPendingErrorState(
        message: String,
        deviceAuthorizationSuppressed: Boolean,
    ) = AccountRecoveryState(
        phase = AccountRecoveryPhase.PENDING,
        message = "$message The exact saved request remains available for retry or confirmed discard.",
        retryAvailable = true,
        discardAvailable = true,
        deviceAuthorizationSuppressed = deviceAuthorizationSuppressed,
    )

    private fun authRequiredState(
        message: String,
        retryAvailable: Boolean = false,
        discardAvailable: Boolean = false,
    ) = AccountRecoveryState(
        phase = AccountRecoveryPhase.AUTH_REQUIRED,
        message = message,
        retryAvailable = retryAvailable,
        discardAvailable = discardAvailable,
    )

    private fun errorState(message: String) = AccountRecoveryState(
        phase = AccountRecoveryPhase.ERROR,
        message = message,
    )

    private fun initialState(): AccountRecoveryState = if (!operationAllowed()) {
        lockedState()
    } else {
        when (val journal = store.read().accountRecoveryJournal) {
            is StoredAccountRecoveryJournal.RepairRequired -> AccountRecoveryState(
                phase = AccountRecoveryPhase.REPAIR_REQUIRED,
                message = "Saved recovery state requires owner-confirmed repair.",
                discardAvailable = true,
                repairRequired = true,
                deviceAuthorizationSuppressed = true,
            )
            is StoredAccountRecoveryJournal.DisclosurePending -> disclosureState(journal)
            is StoredAccountRecoveryJournal.IssuancePending -> AccountRecoveryState(
                phase = AccountRecoveryPhase.PENDING,
                message = "An exact account recovery request is ready for retry.",
                retryAvailable = true,
                discardAvailable = true,
            )
            is StoredAccountRecoveryJournal.ConsumptionPending -> AccountRecoveryState(
                phase = AccountRecoveryPhase.PENDING,
                message = "An exact account recovery request is ready for retry. " +
                    "API-bound changes stay paused until it is resolved.",
                retryAvailable = true,
                discardAvailable = true,
                deviceAuthorizationSuppressed = true,
            )
            is StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation ->
                AccountRecoveryState(
                    phase = AccountRecoveryPhase.PENDING,
                    message = "Recovery is committed and ready for local installation.",
                    retryAvailable = true,
                    deviceAuthorizationSuppressed = true,
                )
            null -> AccountRecoveryState(
                phase = AccountRecoveryPhase.NOT_AVAILABLE,
                message = "Refresh to check account recovery.",
            )
        }
    }

    private fun lockedState() = AccountRecoveryState(
        phase = AccountRecoveryPhase.LOCKED,
        message = "Unlock DayWeave to manage account recovery.",
    )

    private fun publishInitial(generation: Long) = publish(generation, initialState())

    private fun publish(
        generation: Long,
        next: AccountRecoveryState,
        advance: Boolean = false,
    ): Boolean = synchronized(presentationLock) {
        if (!operationAllowed() || generation != presentationGeneration) return@synchronized false
        mutableState.value = next
        if (advance) presentationGeneration = nextGeneration(presentationGeneration)
        true
    }

    private suspend fun <T> withPresentationOperation(
        expectedGeneration: Long? = null,
        block: suspend (Long) -> T,
    ): T? {
        val job = currentCoroutineContext()[Job] ?: return null
        val generation = synchronized(presentationLock) {
            if (
                !operationAllowed() ||
                expectedGeneration != null && expectedGeneration != presentationGeneration
            ) return@synchronized null
            activeJobs += job
            presentationGeneration
        } ?: return null
        return try {
            block(generation)
        } finally {
            synchronized(presentationLock) { activeJobs -= job }
        }
    }

    private fun nextGeneration(value: Long): Long = if (value == Long.MAX_VALUE) 1 else value + 1

    private data class ConsumptionTuple(
        val clientInstanceId: String,
        val sessionId: String,
        val accessToken: String,
        val refreshToken: String,
        val successorId: String,
        val successorCode: String,
    )

    private companion object {
        const val MAX_GENERATION_ATTEMPTS = 32
    }
}
