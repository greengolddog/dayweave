package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.GoogleCalendarOutboundApprovalCapability
import com.greengolddog.dayweave.model.GoogleCalendarOutboundCandidate
import com.greengolddog.dayweave.model.GoogleCalendarOutboundJournal
import com.greengolddog.dayweave.model.GoogleCalendarOutboundPreviewSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundStage
import com.greengolddog.dayweave.model.GoogleCalendarOutboundTarget
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.googleCalendarOutboundCandidate
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleCalendarOutboundApiException
import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.network.GoogleCalendarOutboundTransport
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.util.Locale
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class GoogleCalendarOutboundPhase {
    PRIVACY_PROTECTED,
    NOT_CONFIGURED,
    READY,
    PREVIEWING,
    AWAITING_APPROVAL,
    APPROVING,
    ENQUEUEING,
    RESPONSE_UNKNOWN,
    EXPIRED,
    ACCEPTED,
    AUTH_REQUIRED,
    OFFLINE,
    RECOVERY_REQUIRED,
    ERROR,
}

data class GoogleCalendarOutboundTargetOption(
    val target: GoogleCalendarOutboundTarget,
    val displayName: String,
) {
    override fun toString(): String =
        "GoogleCalendarOutboundTargetOption(target=<redacted>, displayName=<redacted>)"
}

/** Opaque proof that the user is approving the exact preview currently on screen. */
class GoogleCalendarOutboundApprovalConfirmation internal constructor(
    internal val recoveryId: String,
    internal val operationGeneration: Long,
    internal val configurationId: String,
    internal val previewId: String,
    internal val previewHash: String,
) {
    override fun toString(): String =
        "GoogleCalendarOutboundApprovalConfirmation(<redacted>)"
}

data class GoogleCalendarOutboundState(
    val phase: GoogleCalendarOutboundPhase,
    val message: String,
    val preview: GoogleCalendarOutboundPreviewSnapshot? = null,
    val hasPendingRecovery: Boolean = false,
    val acceptedWasReplay: Boolean? = null,
    val isBusy: Boolean = false,
    val configurationId: String? = null,
) {
    override fun toString(): String =
        "GoogleCalendarOutboundState(phase=$phase, preview=<redacted>, " +
            "hasPendingRecovery=$hasPendingRecovery, acceptedWasReplay=$acceptedWasReplay, " +
            "isBusy=$isBusy, configuration=<redacted>)"
}

enum class GoogleCalendarOutboundOutcome {
    PREVIEW_READY,
    ACCEPTED,
    RECOVERED,
    PENDING,
    EXPIRED,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    RECOVERY_REQUIRED,
    FAILED,
}

/**
 * Privacy-fenced, crash-safe orchestration for one explicitly reviewed Google mutation.
 *
 * No schedule block or provider payload is accepted from the caller. The coordinator derives the
 * current canonical item and writable target from authoritative cached state, persists every
 * consequential stage in SQLCipher, and never retries the one-shot approval ceremony.
 */
class GoogleCalendarOutboundCoordinator(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: GoogleCalendarOutboundTransport,
    private val googleAccountState: () -> GoogleAccountState,
    private val googleImportState: () -> GoogleCalendarImportState,
    private val now: () -> Instant = Instant::now,
    private val newUuid: () -> UUID = UUID::randomUUID,
    private val operationAllowed: () -> Boolean = { true },
) {
    private val operationMutex = Mutex()
    private val presentationMonitor = Any()
    private val lifecycleGeneration = AtomicLong(1)
    private val operationSequence = AtomicLong(0)
    private val mutableState = MutableStateFlow(initialState())
    val state: StateFlow<GoogleCalendarOutboundState> = mutableState.asStateFlow()

    fun quarantineBindingState() {
        synchronized(presentationMonitor) {
            lifecycleGeneration.updateAndGet(Math::incrementExact)
            mutableState.value = initialState()
        }
    }

    fun hasCredentialRecoveryBlocker(): Boolean =
        plannerStore.state.value.pendingGoogleCalendarOutbound != null

    /** Clears only settled/error presentation; durable recovery is never affected. */
    fun resetPresentationWithoutRecovery(): Boolean {
        if (!operationAllowed() || state.value.isBusy) return false
        if (plannerStore.state.value.pendingGoogleCalendarOutbound != null) return false
        synchronized(presentationMonitor) {
            if (
                state.value.isBusy ||
                plannerStore.state.value.pendingGoogleCalendarOutbound != null
            ) {
                return false
            }
            mutableState.value = initialState()
        }
        return true
    }

    fun approvalConfirmation(): GoogleCalendarOutboundApprovalConfirmation? {
        if (!operationAllowed()) return null
        val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound ?: return null
        val preview = journal.preview ?: return null
        if (
            journal.stage != GoogleCalendarOutboundStage.PREVIEWED ||
            !journal.isValidAt(now()) ||
            !now().isBefore(Instant.parse(preview.expiresAt)) ||
            state.value.preview != preview ||
            !candidateAndTargetRemainCurrent(journal)
        ) {
            return null
        }
        return GoogleCalendarOutboundApprovalConfirmation(
            recoveryId = journal.recoveryId,
            operationGeneration = journal.operationGeneration,
            configurationId = journal.configurationId,
            previewId = preview.id,
            previewHash = preview.previewHash,
        )
    }

    /** Returns only currently proven writable targets for this exact entity and operation. */
    fun targetsFor(itemId: String): List<GoogleCalendarOutboundTargetOption> {
        if (!operationAllowed()) return emptyList()
        val planner = plannerStore.durableState.value ?: return emptyList()
        val candidate = planner.googleCalendarOutboundCandidate(itemId) ?: return emptyList()
        if (
            planner.pendingGoogleCalendarOutbound != null ||
            planner.pendingGoogleSchedulePublication?.stage?.let {
                it != GoogleSchedulePublicationStage.ACCEPTED
            } == true
        ) return emptyList()
        val snapshot = credentialStore.snapshot()
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            snapshot.configurationId == null ||
            accounts.phase != GoogleAccountPhase.CONNECTED ||
            accounts.isBusy || accounts.authorization != null || imports.isBusy ||
            accounts.configurationId != snapshot.configurationId ||
            imports.configurationId != snapshot.configurationId
        ) {
            return emptyList()
        }
        return accounts.accounts.asSequence()
            .filter { account -> account.hasWriteScopeFor(candidate.entityKind) }
            .flatMap { account ->
                imports.accounts[account.id]?.collections.orEmpty().asSequence().mapNotNull {
                    collection ->
                    currentTarget(
                        account = account,
                        collection = collection,
                        entityKind = candidate.entityKind,
                        operation = candidate.operation,
                    )?.let { target ->
                        GoogleCalendarOutboundTargetOption(
                            target = target,
                            displayName = "${account.label} · ${collection.displayName}",
                        )
                    }
                }
            }
            .sortedBy { it.displayName.lowercase(Locale.getDefault()) }
            .toList()
    }

    /** Account-qualified label for the exact saved destination, without exposing provider IDs. */
    fun pendingDestinationOption(): GoogleCalendarOutboundTargetOption? {
        if (!operationAllowed()) return null
        val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound ?: return null
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            accounts.configurationId != journal.configurationId ||
            imports.configurationId != journal.configurationId
        ) {
            return null
        }
        val account = accounts.accounts.singleOrNull { it.id == journal.accountId } ?: return null
        val collection = imports.accounts[journal.accountId]
            ?.collections?.singleOrNull { it.id == journal.collectionId } ?: return null
        val target = currentTarget(
            account = account,
            collection = collection,
            entityKind = journal.entityKind,
            operation = journal.operation,
        ) ?: return null
        if (journal.preview?.collectionRevision?.let { it != target.collectionRevision } == true) {
            return null
        }
        return GoogleCalendarOutboundTargetOption(
            target = target,
            displayName = "${account.label} · ${collection.displayName}",
        )
    }

    suspend fun preparePreview(
        itemId: String,
        requestedTarget: GoogleCalendarOutboundTarget,
    ): GoogleCalendarOutboundOutcome = withBoundOperation { lifecycle, binding ->
        operationMutex.withLock {
            requireCurrent(lifecycle, binding)
            val current = plannerStore.durableState.value
                ?: return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                    "Encrypted planner state is not ready for Google publication.",
                    GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                )
            if (current.pendingGoogleCalendarOutbound != null) {
                presentJournal(lifecycle, current.pendingGoogleCalendarOutbound)
                return@withLock GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED
            }
            if (
                current.pendingGoogleSchedulePublication?.stage?.let {
                    it != GoogleSchedulePublicationStage.ACCEPTED
                } == true
            ) {
                return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                    "Recover the generated-schedule publication before publishing another change.",
                    GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                )
            }
            val candidate = current.googleCalendarOutboundCandidate(itemId)
                ?: return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.ERROR,
                    "Only a supported synced app-authored event or task can be published.",
                    GoogleCalendarOutboundOutcome.FAILED,
                )
            val target = requireCurrentTarget(
                requested = requestedTarget,
                expectedConfigurationId = binding.configurationId,
                expectedEntityKind = candidate.entityKind,
                expectedOperation = candidate.operation,
            )
                ?: return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.ERROR,
                    "That Google publication destination is no longer available. Refresh Google sources.",
                    GoogleCalendarOutboundOutcome.FAILED,
                )
            val createdAt = now()
            val journal = GoogleCalendarOutboundJournal(
                recoveryId = newUuid().toString(),
                operationGeneration = nextOperationGeneration(),
                configurationId = binding.configurationId,
                apiBaseUrl = binding.apiBaseUrl,
                accountId = target.accountId,
                collectionId = target.collectionId,
                itemId = candidate.itemId,
                expectedItemRevision = candidate.expectedItemRevision,
                entityKind = candidate.entityKind,
                operation = candidate.operation,
                intentExpiresAt = createdAt
                    .plus(GoogleCalendarOutboundJournal.MAXIMUM_INTENT_LIFETIME)
                    .toString(),
                createdAt = createdAt.toString(),
            )
            setState(
                lifecycle,
                GoogleCalendarOutboundState(
                    phase = GoogleCalendarOutboundPhase.PREVIEWING,
                    message = "Preparing the exact ${candidate.serviceTitle()} change…",
                    hasPendingRecovery = true,
                    isBusy = true,
                    configurationId = binding.configurationId,
                ),
            )
            if (!plannerStore.replaceGoogleCalendarOutboundJournal(null, journal)) {
                return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                    "The encrypted Google publication intent could not be saved.",
                    GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                )
            }
            requireCurrent(lifecycle, binding)
            val remote = transport.preview(
                configuration = binding.configuration,
                accountId = journal.accountId,
                collectionId = journal.collectionId,
                itemId = journal.itemId,
                expectedItemRevision = journal.expectedItemRevision,
                operation = journal.operation,
            )
            requireCurrent(lifecycle, binding)
            if (
                remote.collectionRevision != target.collectionRevision ||
                !now().isBefore(Instant.parse(remote.expiresAt))
            ) {
                throw InvalidGoogleOutboundResponseException()
            }
            val previewed = journal.recordingPreview(remote)
            if (!plannerStore.replaceGoogleCalendarOutboundJournal(journal, previewed)) {
                throw GoogleOutboundRecoveryChangedException()
            }
            requireCurrent(lifecycle, binding)
            presentJournal(lifecycle, previewed)
            GoogleCalendarOutboundOutcome.PREVIEW_READY
        }
    }

    suspend fun approveAndEnqueue(
        confirmation: GoogleCalendarOutboundApprovalConfirmation,
    ): GoogleCalendarOutboundOutcome = withBoundOperation { lifecycle, binding ->
        operationMutex.withLock {
            requireCurrent(lifecycle, binding)
            val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound
                ?: return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                    "The encrypted Google publication preview is unavailable.",
                    GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                )
            val preview = journal.preview
            if (
                journal.stage != GoogleCalendarOutboundStage.PREVIEWED || preview == null ||
                confirmation.recoveryId != journal.recoveryId ||
                confirmation.operationGeneration != journal.operationGeneration ||
                confirmation.configurationId != journal.configurationId ||
                confirmation.previewId != preview.id ||
                confirmation.previewHash != preview.previewHash ||
                binding.configurationId != journal.configurationId ||
                binding.apiBaseUrl != journal.apiBaseUrl ||
                !now().isBefore(Instant.parse(preview.expiresAt)) ||
                !candidateAndTargetRemainCurrent(journal)
            ) {
                return@withLock failure(
                    lifecycle,
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                    "The item, destination, or preview changed. Nothing was approved.",
                    GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                )
            }
            setState(
                lifecycle,
                state.value.copy(
                    phase = GoogleCalendarOutboundPhase.APPROVING,
                    message = "Creating one short-lived approval for this exact preview…",
                    preview = null,
                    hasPendingRecovery = true,
                    isBusy = true,
                ),
            )
            val attempted = journal.recordingApprovalAttempt()
            if (!plannerStore.replaceGoogleCalendarOutboundJournal(journal, attempted)) {
                throw GoogleOutboundRecoveryChangedException()
            }
            requireCurrent(lifecycle, binding)
            val remoteApproval = transport.approve(
                configuration = binding.configuration,
                accountId = attempted.accountId,
                previewId = preview.id,
                expectedPreviewHash = preview.previewHash,
            )
            requireCurrent(lifecycle, binding)
            if (!now().isBefore(Instant.parse(remoteApproval.expiresAt))) {
                throw InvalidGoogleOutboundResponseException()
            }
            val approved = attempted.recordingApproval(remoteApproval)
            if (!plannerStore.replaceGoogleCalendarOutboundJournal(attempted, approved)) {
                throw GoogleOutboundRecoveryChangedException()
            }
            requireCurrent(lifecycle, binding)
            enqueueApproved(lifecycle, binding, approved)
        }
    }

    /** Replays preview or enqueue only. A recovered preview is never approved automatically. */
    suspend fun recoverPending(): GoogleCalendarOutboundOutcome =
        withBoundOperation { lifecycle, binding ->
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound
                    ?: run {
                        setState(lifecycle, readyState(binding))
                        return@withLock GoogleCalendarOutboundOutcome.RECOVERED
                    }
                if (
                    journal.configurationId != binding.configurationId ||
                    journal.apiBaseUrl != binding.apiBaseUrl ||
                    !journal.isValidAt(now())
                ) {
                    return@withLock failure(
                        lifecycle,
                        GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                        "The saved Google publication belongs to another API connection.",
                        GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                    )
                }
                if (
                    journal.stage != GoogleCalendarOutboundStage.APPROVED &&
                    !now().isBefore(journal.authorityExpiresAt())
                ) {
                    presentJournal(lifecycle, journal)
                    return@withLock GoogleCalendarOutboundOutcome.EXPIRED
                }
                when (journal.stage) {
                    GoogleCalendarOutboundStage.INTENT -> {
                        val recoveredTarget = currentCandidateAndTarget(journal)?.second
                            ?: return@withLock failure(
                                lifecycle,
                                GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                                "The saved item or Publish destination changed. Nothing was sent.",
                                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                            )
                        setState(
                            lifecycle,
                            GoogleCalendarOutboundState(
                                phase = GoogleCalendarOutboundPhase.PREVIEWING,
                                message = "Recovering the saved ${journal.serviceTitle()} preview…",
                                hasPendingRecovery = true,
                                isBusy = true,
                                configurationId = binding.configurationId,
                            ),
                        )
                        val remote = transport.preview(
                            configuration = binding.configuration,
                            accountId = journal.accountId,
                            collectionId = journal.collectionId,
                            itemId = journal.itemId,
                            expectedItemRevision = journal.expectedItemRevision,
                            operation = journal.operation,
                        )
                        requireCurrent(lifecycle, binding)
                        if (
                            currentCandidateAndTarget(journal)?.second != recoveredTarget ||
                            remote.collectionRevision != recoveredTarget.collectionRevision ||
                            !now().isBefore(Instant.parse(remote.expiresAt))
                        ) {
                            throw InvalidGoogleOutboundResponseException()
                        }
                        val previewed = journal.recordingPreview(remote)
                        if (!plannerStore.replaceGoogleCalendarOutboundJournal(journal, previewed)) {
                            throw GoogleOutboundRecoveryChangedException()
                        }
                        requireCurrent(lifecycle, binding)
                        presentJournal(lifecycle, previewed)
                        GoogleCalendarOutboundOutcome.PREVIEW_READY
                    }
                    GoogleCalendarOutboundStage.PREVIEWED -> {
                        presentJournal(lifecycle, journal)
                        GoogleCalendarOutboundOutcome.PREVIEW_READY
                    }
                    GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED -> {
                        presentJournal(lifecycle, journal)
                        GoogleCalendarOutboundOutcome.PENDING
                    }
                    GoogleCalendarOutboundStage.APPROVED ->
                        enqueueApproved(lifecycle, binding, journal)
                }
            }
        }

    suspend fun discardExpiredRecovery(): Boolean {
        if (!operationAllowed()) return false
        return operationMutex.withLock {
            val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound
                ?: return@withLock true
            if (!plannerStore.discardExpiredGoogleCalendarOutbound(journal, now())) {
                return@withLock false
            }
            synchronized(presentationMonitor) {
                mutableState.value = initialState()
            }
            true
        }
    }

    private suspend fun enqueueApproved(
        lifecycle: Long,
        binding: BoundGoogleOutboundConfiguration,
        journal: GoogleCalendarOutboundJournal,
    ): GoogleCalendarOutboundOutcome {
        require(journal.stage == GoogleCalendarOutboundStage.APPROVED)
        setState(
            lifecycle,
            GoogleCalendarOutboundState(
                phase = GoogleCalendarOutboundPhase.ENQUEUEING,
                message = "Saving the approved ${journal.serviceTitle()} change to the durable outbox…",
                hasPendingRecovery = true,
                isBusy = true,
                configurationId = binding.configurationId,
            ),
        )
        val accepted = transport.enqueue(
            configuration = binding.configuration,
            accountId = journal.accountId,
            collectionId = journal.collectionId,
            itemId = journal.itemId,
            expectedItemRevision = journal.expectedItemRevision,
            operation = journal.operation,
            approvalCapability = requireNotNull(journal.approvalCapability).value,
        )
        requireCurrent(lifecycle, binding)
        if (!plannerStore.clearGoogleCalendarOutboundAfterAcceptance(journal)) {
            throw GoogleOutboundRecoveryChangedException()
        }
        requireCurrent(lifecycle, binding)
        setState(
            lifecycle,
            GoogleCalendarOutboundState(
                phase = GoogleCalendarOutboundPhase.ACCEPTED,
                message = if (accepted.replayed) {
                    "The previously queued ${journal.serviceTitle()} change was recovered."
                } else {
                    "The reviewed ${journal.serviceTitle()} change is queued for publication."
                },
                hasPendingRecovery = false,
                acceptedWasReplay = accepted.replayed,
                configurationId = binding.configurationId,
            ),
        )
        return GoogleCalendarOutboundOutcome.ACCEPTED
    }

    private fun requireCurrentTarget(
        requested: GoogleCalendarOutboundTarget,
        expectedConfigurationId: String,
        expectedEntityKind: GoogleCalendarOutboundEntityKind,
        expectedOperation: GoogleCalendarOutboundOperation,
    ): GoogleCalendarOutboundTarget? {
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            accounts.phase != GoogleAccountPhase.CONNECTED ||
            accounts.isBusy || accounts.authorization != null || imports.isBusy ||
            accounts.configurationId != expectedConfigurationId ||
            imports.configurationId != expectedConfigurationId
        ) {
            return null
        }
        val account = accounts.accounts.singleOrNull { it.id == requested.accountId }
            ?: return null
        val collection = imports.accounts[requested.accountId]
            ?.collections?.singleOrNull { it.id == requested.collectionId } ?: return null
        return currentTarget(
            account = account,
            collection = collection,
            entityKind = expectedEntityKind,
            operation = expectedOperation,
        )?.takeIf { it == requested }
    }

    private fun currentTarget(
        account: GoogleAccountSummary,
        collection: GoogleImportCollectionState,
        entityKind: GoogleCalendarOutboundEntityKind,
        operation: GoogleCalendarOutboundOperation,
    ): GoogleCalendarOutboundTarget? = runCatching {
        require(account.hasWriteScopeFor(entityKind))
        require(collection.accountId == account.id)
        require(
            collection.kind == when (entityKind) {
                GoogleCalendarOutboundEntityKind.CALENDAR_EVENT ->
                    RemoteGoogleCollectionKind.CALENDAR
                GoogleCalendarOutboundEntityKind.TASK ->
                    RemoteGoogleCollectionKind.TASK_LIST
            },
        )
        require(collection.selected && !collection.providerDeleted)
        require(collection.syncRole == RemoteGoogleSyncRole.WRITABLE)
        require(collection.revision > 0)
        if (entityKind == GoogleCalendarOutboundEntityKind.CALENDAR_EVENT) {
            require(
                collection.providerAccessRole?.lowercase(Locale.ROOT) in setOf("owner", "writer"),
            )
        }
        GoogleCalendarOutboundTarget(
            accountId = account.id,
            collectionId = collection.id,
            collectionRevision = collection.revision,
            entityKind = entityKind,
            operation = operation,
        )
    }.getOrNull()

    private fun candidateAndTargetRemainCurrent(journal: GoogleCalendarOutboundJournal): Boolean {
        val (_, target) = currentCandidateAndTarget(journal) ?: return false
        return target.collectionRevision == journal.preview?.collectionRevision
    }

    private fun currentCandidateAndTarget(
        journal: GoogleCalendarOutboundJournal,
    ): Pair<GoogleCalendarOutboundCandidate, GoogleCalendarOutboundTarget>? {
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            accounts.phase != GoogleAccountPhase.CONNECTED ||
            accounts.isBusy || accounts.authorization != null || imports.isBusy ||
            accounts.configurationId != journal.configurationId ||
            imports.configurationId != journal.configurationId
        ) {
            return null
        }
        val candidate = plannerStore.durableState.value
            ?.googleCalendarOutboundCandidate(journal.itemId) ?: return null
        if (
            candidate.expectedItemRevision != journal.expectedItemRevision ||
            candidate.entityKind != journal.entityKind ||
            candidate.operation != journal.operation
        ) return null
        val account = accounts.accounts.singleOrNull { it.id == journal.accountId }
            ?: return null
        val collection = imports.accounts[journal.accountId]
            ?.collections?.singleOrNull { it.id == journal.collectionId } ?: return null
        val target = currentTarget(
            account = account,
            collection = collection,
            entityKind = journal.entityKind,
            operation = journal.operation,
        ) ?: return null
        return candidate to target
    }

    private fun presentJournal(lifecycle: Long, journal: GoogleCalendarOutboundJournal) {
        val currentTime = now()
        val next = when {
            journal.stage == GoogleCalendarOutboundStage.PREVIEWED &&
                currentTime.isBefore(requireNotNull(journal.preview).let { Instant.parse(it.expiresAt) }) ->
                GoogleCalendarOutboundState(
                    phase = GoogleCalendarOutboundPhase.AWAITING_APPROVAL,
                    message = "Review the exact ${journal.serviceTitle()} change, then approve it explicitly.",
                    preview = journal.preview,
                    hasPendingRecovery = true,
                    configurationId = journal.configurationId,
                )
            journal.stage == GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED &&
                currentTime.isBefore(journal.safeDiscardAt()) ->
                GoogleCalendarOutboundState(
                    phase = GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN,
                    message = "The one-time approval response is unknown. It will not be retried; wait for safe expiry before discarding it.",
                    hasPendingRecovery = true,
                    configurationId = journal.configurationId,
                )
            !currentTime.isBefore(journal.safeDiscardAt()) ->
                GoogleCalendarOutboundState(
                    phase = GoogleCalendarOutboundPhase.EXPIRED,
                    message = "The saved Google publication authority expired and can now be discarded safely.",
                    hasPendingRecovery = true,
                    configurationId = journal.configurationId,
                )
            else -> GoogleCalendarOutboundState(
                phase = GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                message = "Recover the saved ${journal.serviceTitle()} publication before starting another.",
                hasPendingRecovery = true,
                configurationId = journal.configurationId,
            )
        }
        setState(lifecycle, next)
    }

    private suspend fun withBoundOperation(
        operation: suspend (
            lifecycle: Long,
            binding: BoundGoogleOutboundConfiguration,
        ) -> GoogleCalendarOutboundOutcome,
    ): GoogleCalendarOutboundOutcome {
        val lifecycle = lifecycleGeneration.get()
        val binding = authenticatedBinding(lifecycle) ?: return bindingFailureOutcome()
        val ticket = try {
            binding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            quarantineBindingState()
            return GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED
        }
        return try {
            operation(lifecycle, binding)
        } catch (error: CancellationException) {
            retainRecoveryAfterFailure(lifecycle, binding, error)
            throw error
        } catch (_: ApiBindingChangedException) {
            quarantineBindingState()
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED
        } catch (_: StaleGoogleOutboundOperationException) {
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED
        } catch (error: Exception) {
            retainRecoveryAfterFailure(lifecycle, binding, error)
        } finally {
            ticket.release()
        }
    }

    private fun authenticatedBinding(lifecycle: Long): BoundGoogleOutboundConfiguration? {
        if (!operationAllowed()) return null
        val snapshot = credentialStore.snapshot()
        if (!snapshot.hasBearerToken || snapshot.configurationId == null || snapshot.baseUrl == null) {
            setState(lifecycle, initialState())
            return null
        }
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            setState(
                lifecycle,
                GoogleCalendarOutboundState(
                    phase = GoogleCalendarOutboundPhase.AUTH_REQUIRED,
                    message = "Reconnect the DayWeave API before publishing to Google.",
                    hasPendingRecovery = hasCredentialRecoveryBlocker(),
                    configurationId = snapshot.configurationId,
                ),
            )
            return null
        } ?: return null
        if (
            configuration.configurationId != snapshot.configurationId ||
            configuration.baseUrl.toString() != snapshot.baseUrl
        ) {
            return null
        }
        return BoundGoogleOutboundConfiguration(
            snapshot = snapshot,
            configuration = configuration,
            configurationId = requireNotNull(snapshot.configurationId),
            apiBaseUrl = requireNotNull(snapshot.baseUrl),
        )
    }

    private fun requireCurrent(
        lifecycle: Long,
        binding: BoundGoogleOutboundConfiguration,
    ) {
        if (
            !operationAllowed() || lifecycleGeneration.get() != lifecycle ||
            !sameBinding(credentialStore.snapshot(), binding.snapshot)
        ) {
            throw StaleGoogleOutboundOperationException()
        }
    }

    private fun setState(lifecycle: Long, next: GoogleCalendarOutboundState) {
        synchronized(presentationMonitor) {
            if (operationAllowed() && lifecycleGeneration.get() == lifecycle) {
                mutableState.value = next
            }
        }
    }

    private fun failure(
        lifecycle: Long,
        phase: GoogleCalendarOutboundPhase,
        message: String,
        outcome: GoogleCalendarOutboundOutcome,
    ): GoogleCalendarOutboundOutcome {
        setState(
            lifecycle,
            GoogleCalendarOutboundState(
                phase = phase,
                message = message,
                hasPendingRecovery = hasCredentialRecoveryBlocker(),
                configurationId = credentialStore.snapshot().configurationId,
            ),
        )
        return outcome
    }

    private fun retainRecoveryAfterFailure(
        lifecycle: Long,
        binding: BoundGoogleOutboundConfiguration,
        error: Exception,
    ): GoogleCalendarOutboundOutcome {
        val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound
        val (phase, message, outcome) = when (error) {
            is GoogleCalendarOutboundApiException.Authentication -> Triple(
                GoogleCalendarOutboundPhase.AUTH_REQUIRED,
                "Google publication needs DayWeave API authentication.",
                GoogleCalendarOutboundOutcome.AUTH_REQUIRED,
            )
            is GoogleCalendarOutboundApiException.Conflict,
            is GoogleCalendarOutboundApiException.NotFound,
            is GoogleCalendarOutboundApiException.Validation,
            is GoogleCalendarOutboundApiException.InvalidResponse,
            is InvalidGoogleOutboundResponseException,
            is GoogleOutboundRecoveryChangedException,
            -> Triple(
                if (journal?.canDiscardExpiredAt(now()) == true) {
                    GoogleCalendarOutboundPhase.EXPIRED
                } else {
                    GoogleCalendarOutboundPhase.RECOVERY_REQUIRED
                },
                "The saved Google change is stale or no longer authorized. Recovery remains saved.",
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            )
            is IOException -> Triple(
                GoogleCalendarOutboundPhase.OFFLINE,
                "Offline or server unavailable · the exact Google operation remains saved.",
                GoogleCalendarOutboundOutcome.PENDING,
            )
            else -> Triple(
                GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
                "The Google operation could not be verified. Recovery remains saved.",
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            )
        }
        setState(
            lifecycle,
            GoogleCalendarOutboundState(
                phase = phase,
                message = message,
                hasPendingRecovery = journal != null,
                configurationId = binding.configurationId,
            ),
        )
        return outcome
    }

    private fun presentStoredJournalIfAvailable() {
        val journal = plannerStore.durableState.value?.pendingGoogleCalendarOutbound ?: return
        presentJournal(lifecycleGeneration.get(), journal)
    }

    private fun bindingFailureOutcome(): GoogleCalendarOutboundOutcome {
        presentStoredJournalIfAvailable()
        return if (credentialStore.snapshot().hasBearerToken) {
            GoogleCalendarOutboundOutcome.AUTH_REQUIRED
        } else {
            GoogleCalendarOutboundOutcome.NOT_CONFIGURED
        }
    }

    private fun readyState(binding: BoundGoogleOutboundConfiguration) =
        GoogleCalendarOutboundState(
            phase = GoogleCalendarOutboundPhase.READY,
            message = "Choose a supported synced event or task and review its Google change.",
            configurationId = binding.configurationId,
        )

    private fun initialState(): GoogleCalendarOutboundState = when {
        !operationAllowed() -> GoogleCalendarOutboundState(
            phase = GoogleCalendarOutboundPhase.PRIVACY_PROTECTED,
            message = "Google publication details are hidden while DayWeave is locked.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleCalendarOutbound != null,
        )
        !credentialStore.snapshot().hasBearerToken -> GoogleCalendarOutboundState(
            phase = GoogleCalendarOutboundPhase.NOT_CONFIGURED,
            message = "Connect the DayWeave API before publishing to Google.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleCalendarOutbound != null,
            configurationId = credentialStore.snapshot().configurationId,
        )
        else -> GoogleCalendarOutboundState(
            phase = GoogleCalendarOutboundPhase.READY,
            message = "Choose a supported synced event or task and review its Google change.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleCalendarOutbound != null,
            configurationId = credentialStore.snapshot().configurationId,
        )
    }

    private fun nextOperationGeneration(): Long = operationSequence.updateAndGet(Math::incrementExact)

    private data class BoundGoogleOutboundConfiguration(
        val snapshot: ApiConnectionSnapshot,
        val configuration: AuthenticatedApiConfiguration,
        val configurationId: String,
        val apiBaseUrl: String,
    )

    private class StaleGoogleOutboundOperationException : IOException(
        "Google outbound operation changed",
    )

    private class InvalidGoogleOutboundResponseException : IOException(
        "Google outbound response was invalid",
    )

    private class GoogleOutboundRecoveryChangedException : IOException(
        "Google outbound recovery changed",
    )

    private companion object {
        fun GoogleAccountSummary.hasWriteScopeFor(
            entityKind: GoogleCalendarOutboundEntityKind,
        ): Boolean = status == "active" && syncEnabled && when (entityKind) {
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> hasCalendarWriteScope
            GoogleCalendarOutboundEntityKind.TASK -> hasTasksWriteScope
        }

        fun GoogleCalendarOutboundJournal.serviceTitle(): String = when (entityKind) {
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> "Google Calendar"
            GoogleCalendarOutboundEntityKind.TASK -> "Google Tasks"
        }

        fun GoogleCalendarOutboundCandidate.serviceTitle(): String = when (entityKind) {
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT -> "Google Calendar"
            GoogleCalendarOutboundEntityKind.TASK -> "Google Tasks"
        }

        fun sameBinding(left: ApiConnectionSnapshot, right: ApiConnectionSnapshot): Boolean =
            left.baseUrl == right.baseUrl && left.hasBearerToken == right.hasBearerToken &&
                left.configurationId == right.configurationId
    }
}
