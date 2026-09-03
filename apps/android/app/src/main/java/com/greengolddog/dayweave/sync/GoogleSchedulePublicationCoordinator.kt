package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.GoogleSchedulePublicationJournal
import com.greengolddog.dayweave.model.GoogleSchedulePublicationPreviewSnapshot
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStatusSnapshot
import com.greengolddog.dayweave.model.GoogleSchedulePublicationTarget
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleCalendarOutboundApiException
import com.greengolddog.dayweave.network.GoogleSchedulePublicationTransport
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.ScheduleGooglePublicationState
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

enum class GoogleSchedulePublicationPhase {
    PRIVACY_PROTECTED,
    NOT_CONFIGURED,
    READY,
    PREVIEWING,
    AWAITING_APPROVAL,
    APPROVING,
    ENQUEUEING,
    CHECKING_STATUS,
    PENDING,
    PARTIALLY_PUBLISHED,
    PUBLISHED,
    CONFLICT,
    FAILED,
    SUPERSEDED,
    RESPONSE_UNKNOWN,
    EXPIRED,
    AUTH_REQUIRED,
    OFFLINE,
    APPROVED_REPLAY_REQUIRED,
    RECOVERY_REQUIRED,
    ERROR,
}

data class GoogleSchedulePublicationTargetOption(
    val target: GoogleSchedulePublicationTarget,
    val displayName: String,
) {
    override fun toString(): String =
        "GoogleSchedulePublicationTargetOption(target=<redacted>, displayName=<redacted>)"
}

class GoogleSchedulePublicationApprovalConfirmation internal constructor(
    internal val recoveryId: String,
    internal val operationGeneration: Long,
    internal val configurationId: String,
    internal val previewId: String,
    internal val previewHash: String,
) {
    override fun toString(): String =
        "GoogleSchedulePublicationApprovalConfirmation(<redacted>)"
}

data class GoogleSchedulePublicationState(
    val phase: GoogleSchedulePublicationPhase,
    val message: String,
    val preview: GoogleSchedulePublicationPreviewSnapshot? = null,
    val status: GoogleSchedulePublicationStatusSnapshot? = null,
    val hasPendingRecovery: Boolean = false,
    val acceptedWasReplay: Boolean? = null,
    val isBusy: Boolean = false,
    val configurationId: String? = null,
) {
    override fun toString(): String =
        "GoogleSchedulePublicationState(phase=$phase, preview=<redacted>, status=$status, " +
            "hasPendingRecovery=$hasPendingRecovery, acceptedWasReplay=$acceptedWasReplay, " +
            "isBusy=$isBusy, configuration=<redacted>)"
}

enum class GoogleSchedulePublicationOutcome {
    PREVIEW_READY,
    ACCEPTED,
    STATUS_UPDATED,
    PENDING,
    EXPIRED,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    RECOVERY_REQUIRED,
    FAILED,
}

/**
 * Crash-safe orchestration for publishing one immutable, already-published generated schedule.
 * Every consequential step is saved in SQLCipher before the next request. Approval is never
 * retried, while enqueue is replayable using the exact saved tuple.
 */
class GoogleSchedulePublicationCoordinator(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: GoogleSchedulePublicationTransport,
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
    val state: StateFlow<GoogleSchedulePublicationState> = mutableState.asStateFlow()

    fun quarantineBindingState() {
        synchronized(presentationMonitor) {
            lifecycleGeneration.updateAndGet(Math::incrementExact)
            mutableState.value = initialState()
        }
    }

    fun hasCredentialRecoveryBlocker(): Boolean =
        plannerStore.state.value.pendingGoogleSchedulePublication?.stage?.let {
            it != GoogleSchedulePublicationStage.ACCEPTED
        } == true

    fun targets(): List<GoogleSchedulePublicationTargetOption> {
        if (!operationAllowed()) return emptyList()
        val planner = plannerStore.durableState.value ?: return emptyList()
        if (!planner.hasCurrentPublishedSchedule() || planner.pendingGoogleSchedulePublication != null) {
            return emptyList()
        }
        val snapshot = credentialStore.snapshot()
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            snapshot.configurationId == null || accounts.phase != GoogleAccountPhase.CONNECTED ||
            accounts.isBusy || accounts.authorization != null ||
            accounts.authorizationRecovery != null ||
            accounts.authorizationRecoveryResetRequired ||
            accounts.authorizationRecoveryDiscardRequired || imports.isBusy ||
            imports.pendingRecoveryCount != 0 ||
            accounts.configurationId != snapshot.configurationId ||
            imports.configurationId != snapshot.configurationId
        ) return emptyList()
        return accounts.accounts.asSequence()
            .filter { it.status == "active" && it.syncEnabled && it.hasCalendarWriteScope }
            .flatMap { account ->
                imports.accounts[account.id]?.collections.orEmpty().asSequence().mapNotNull {
                    collection ->
                    currentTarget(account, collection)?.let { target ->
                        GoogleSchedulePublicationTargetOption(
                            target = target,
                            displayName = "${account.label} · ${collection.displayName}",
                        )
                    }
                }
            }
            .sortedBy { it.displayName.lowercase(Locale.getDefault()) }
            .toList()
    }

    fun pendingDestinationOption(): GoogleSchedulePublicationTargetOption? {
        if (!operationAllowed()) return null
        val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication ?: return null
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            accounts.configurationId != journal.configurationId ||
            imports.configurationId != journal.configurationId
        ) return null
        val account = accounts.accounts.singleOrNull { it.id == journal.accountId } ?: return null
        val collection = imports.accounts[journal.accountId]?.collections
            ?.singleOrNull { it.id == journal.collectionId } ?: return null
        val target = currentTarget(account, collection) ?: return null
        if (journal.preview?.collectionRevision?.let { it != target.collectionRevision } == true) {
            return null
        }
        return GoogleSchedulePublicationTargetOption(
            target,
            "${account.label} · ${collection.displayName}",
        )
    }

    fun approvalConfirmation(): GoogleSchedulePublicationApprovalConfirmation? {
        if (!operationAllowed()) return null
        val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication ?: return null
        val preview = journal.preview ?: return null
        if (
            journal.stage != GoogleSchedulePublicationStage.PREVIEWED ||
            !journal.isValidAt(now()) || !now().isBefore(Instant.parse(preview.expiresAt)) ||
            state.value.preview != preview || !sourceAndTargetRemainCurrent(journal)
        ) return null
        return GoogleSchedulePublicationApprovalConfirmation(
            journal.recoveryId,
            journal.operationGeneration,
            journal.configurationId,
            preview.id,
            preview.previewHash,
        )
    }

    suspend fun preparePreview(
        requestedTarget: GoogleSchedulePublicationTarget,
    ): GoogleSchedulePublicationOutcome = withBoundOperation { lifecycle, binding ->
        operationMutex.withLock {
            requireCurrent(lifecycle, binding)
            val planner = plannerStore.durableState.value
                ?: return@withLock failure(
                    lifecycle,
                    GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                    "Encrypted planner state is not ready.",
                    GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
                )
            planner.pendingGoogleSchedulePublication?.let {
                presentJournal(lifecycle, it)
                return@withLock GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
            }
            val revision = planner.publishedScheduleRevision?.takeIf {
                planner.hasCurrentPublishedSchedule()
            } ?: return@withLock failure(
                lifecycle,
                GoogleSchedulePublicationPhase.ERROR,
                "Publish the current generated schedule in DayWeave before sending it to Google.",
                GoogleSchedulePublicationOutcome.FAILED,
            )
            val target = requireCurrentTarget(requestedTarget, binding.configurationId)
                ?: return@withLock failure(
                    lifecycle,
                    GoogleSchedulePublicationPhase.ERROR,
                    "That writable calendar is no longer available. Refresh Google sources.",
                    GoogleSchedulePublicationOutcome.FAILED,
                )
            val created = now()
            val journal = GoogleSchedulePublicationJournal(
                recoveryId = newUuid().toString(),
                operationGeneration = operationSequence.updateAndGet(Math::incrementExact),
                configurationId = binding.configurationId,
                apiBaseUrl = binding.apiBaseUrl,
                accountId = target.accountId,
                collectionId = target.collectionId,
                expectedScheduleRevisionId = revision.id,
                intentExpiresAt = created.plus(
                    GoogleSchedulePublicationJournal.MAXIMUM_INTENT_LIFETIME,
                ).toString(),
                createdAt = created.toString(),
            )
            setState(
                lifecycle,
                GoogleSchedulePublicationState(
                    GoogleSchedulePublicationPhase.PREVIEWING,
                    "Preparing the exact Google Calendar change set…",
                    hasPendingRecovery = true,
                    isBusy = true,
                    configurationId = binding.configurationId,
                ),
            )
            val intentSaved = plannerStore.replaceGoogleSchedulePublicationJournal(null, journal)
            requireCurrent(lifecycle, binding)
            if (!intentSaved) {
                throw SchedulePublicationRecoveryChangedException()
            }
            val remote = transport.previewSchedulePublication(
                binding.configuration,
                journal.accountId,
                journal.collectionId,
                journal.expectedScheduleRevisionId,
            )
            requireCurrent(lifecycle, binding)
            if (
                remote.collectionRevision != target.collectionRevision ||
                !now().isBefore(Instant.parse(remote.expiresAt))
            ) throw InvalidSchedulePublicationResponseException()
            val previewed = journal.recordingPreview(remote)
            val previewSaved = plannerStore.replaceGoogleSchedulePublicationJournal(
                journal,
                previewed,
            )
            requireCurrent(lifecycle, binding)
            if (!previewSaved) {
                throw SchedulePublicationRecoveryChangedException()
            }
            presentJournal(lifecycle, previewed)
            GoogleSchedulePublicationOutcome.PREVIEW_READY
        }
    }

    suspend fun approveAndEnqueue(
        confirmation: GoogleSchedulePublicationApprovalConfirmation,
    ): GoogleSchedulePublicationOutcome = withBoundOperation { lifecycle, binding ->
        operationMutex.withLock {
            requireCurrent(lifecycle, binding)
            val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
                ?: throw SchedulePublicationRecoveryChangedException()
            val preview = journal.preview ?: throw SchedulePublicationRecoveryChangedException()
            if (
                journal.stage != GoogleSchedulePublicationStage.PREVIEWED ||
                confirmation.recoveryId != journal.recoveryId ||
                confirmation.operationGeneration != journal.operationGeneration ||
                confirmation.configurationId != journal.configurationId ||
                confirmation.previewId != preview.id || confirmation.previewHash != preview.previewHash ||
                binding.configurationId != journal.configurationId ||
                binding.apiBaseUrl != journal.apiBaseUrl ||
                !now().isBefore(Instant.parse(preview.expiresAt)) ||
                !sourceAndTargetRemainCurrent(journal)
            ) {
                return@withLock failure(
                    lifecycle,
                    GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                    "The schedule, destination, or preview changed. Nothing was approved.",
                    GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
                )
            }
            setState(
                lifecycle,
                state.value.copy(
                    phase = GoogleSchedulePublicationPhase.APPROVING,
                    message = "Creating one short-lived approval for this exact change set…",
                    preview = null,
                    isBusy = true,
                ),
            )
            val attempted = journal.recordingApprovalAttempt()
            val attemptSaved = plannerStore.replaceGoogleSchedulePublicationJournal(
                journal,
                attempted,
            )
            requireCurrent(lifecycle, binding)
            if (!attemptSaved) {
                throw SchedulePublicationRecoveryChangedException()
            }
            val approval = transport.approveSchedulePublication(
                binding.configuration,
                attempted.accountId,
                preview.id,
                preview.previewHash,
            )
            requireCurrent(lifecycle, binding)
            val approvalExpiry = Instant.parse(approval.expiresAt)
            val earliestToleratedExpiry = Instant.parse(attempted.createdAt)
                .minus(GoogleSchedulePublicationJournal.MAXIMUM_CLOCK_SKEW)
            if (
                approvalExpiry < earliestToleratedExpiry ||
                approvalExpiry > Instant.parse(preview.expiresAt)
            ) {
                throw InvalidSchedulePublicationResponseException()
            }
            val approved = attempted.recordingApproval(approval)
            val approvalSaved = plannerStore.replaceGoogleSchedulePublicationJournal(
                attempted,
                approved,
            )
            requireCurrent(lifecycle, binding)
            if (!approvalSaved) {
                throw SchedulePublicationRecoveryChangedException()
            }
            enqueueApproved(lifecycle, binding, approved)
        }
    }

    suspend fun recoverPending(): GoogleSchedulePublicationOutcome =
        withBoundOperation { lifecycle, binding ->
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
                    ?: run {
                        setState(lifecycle, readyState(binding))
                        return@withLock GoogleSchedulePublicationOutcome.STATUS_UPDATED
                    }
                if (
                    journal.configurationId != binding.configurationId ||
                    journal.apiBaseUrl != binding.apiBaseUrl || !journal.isValidAt(now())
                ) {
                    return@withLock failure(
                        lifecycle,
                        GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                        "This saved publication belongs to another DayWeave API connection.",
                        GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
                    )
                }
                if (
                    journal.stage != GoogleSchedulePublicationStage.APPROVED &&
                    journal.stage != GoogleSchedulePublicationStage.ACCEPTED &&
                    !now().isBefore(journal.authorityExpiresAt())
                ) {
                    presentJournal(lifecycle, journal)
                    return@withLock GoogleSchedulePublicationOutcome.EXPIRED
                }
                when (journal.stage) {
                    GoogleSchedulePublicationStage.INTENT -> recoverPreview(lifecycle, binding, journal)
                    GoogleSchedulePublicationStage.PREVIEWED -> {
                        presentJournal(lifecycle, journal)
                        GoogleSchedulePublicationOutcome.PREVIEW_READY
                    }
                    GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED -> {
                        presentJournal(lifecycle, journal)
                        GoogleSchedulePublicationOutcome.PENDING
                    }
                    GoogleSchedulePublicationStage.APPROVED -> {
                        // Enqueue is replayable, but it is still an external mutation. Automatic
                        // foreground recovery may surface this state; only the separately confirmed
                        // replay entry point below is allowed to make the request.
                        presentJournal(lifecycle, journal)
                        GoogleSchedulePublicationOutcome.PENDING
                    }
                    GoogleSchedulePublicationStage.ACCEPTED ->
                        refreshAcceptedStatus(lifecycle, binding, journal)
                }
            }
        }

    /** Replays one already-approved exact enqueue after the user confirms that external effect. */
    suspend fun replayApprovedEnqueue(): GoogleSchedulePublicationOutcome =
        withBoundOperation { lifecycle, binding ->
            operationMutex.withLock {
                requireCurrent(lifecycle, binding)
                val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
                    ?: throw SchedulePublicationRecoveryChangedException()
                if (
                    journal.stage != GoogleSchedulePublicationStage.APPROVED ||
                    journal.configurationId != binding.configurationId ||
                    journal.apiBaseUrl != binding.apiBaseUrl || !journal.isValidAt(now())
                ) {
                    presentJournal(lifecycle, journal)
                    return@withLock GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
                }
                enqueueApproved(lifecycle, binding, journal)
            }
        }

    suspend fun refreshStatus(): GoogleSchedulePublicationOutcome = recoverPending()

    suspend fun discardExpiredRecovery(): Boolean {
        val lifecycle = lifecycleGeneration.get()
        if (!operationAllowed()) return false
        return operationMutex.withLock {
            if (!operationAllowed() || lifecycleGeneration.get() != lifecycle) {
                return@withLock false
            }
            val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
                ?: return@withLock true
            val currentTime = now()
            if (!operationAllowed() || lifecycleGeneration.get() != lifecycle) {
                return@withLock false
            }
            if (!plannerStore.discardExpiredGoogleSchedulePublication(journal, currentTime)) {
                return@withLock false
            }
            setState(lifecycle, initialState())
            true
        }
    }

    suspend fun dismissSettled(): Boolean {
        val lifecycle = lifecycleGeneration.get()
        if (!operationAllowed()) return false
        return operationMutex.withLock {
            if (!operationAllowed() || lifecycleGeneration.get() != lifecycle) {
                return@withLock false
            }
            val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
                ?: return@withLock true
            if (!operationAllowed() || lifecycleGeneration.get() != lifecycle) {
                return@withLock false
            }
            if (!plannerStore.dismissSettledGoogleSchedulePublication(journal)) return@withLock false
            setState(lifecycle, initialState())
            true
        }
    }

    private suspend fun recoverPreview(
        lifecycle: Long,
        binding: BoundSchedulePublicationConfiguration,
        journal: GoogleSchedulePublicationJournal,
    ): GoogleSchedulePublicationOutcome {
        val target = currentSourceAndTarget(journal)?.second
            ?: return failure(
                lifecycle,
                GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                "The published schedule or destination changed. Nothing was sent.",
                GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
            )
        setState(
            lifecycle,
            GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.PREVIEWING,
                "Recovering the saved schedule preview…",
                hasPendingRecovery = true,
                isBusy = true,
                configurationId = binding.configurationId,
            ),
        )
        requireCurrent(lifecycle, binding)
        val remote = transport.previewSchedulePublication(
            binding.configuration,
            journal.accountId,
            journal.collectionId,
            journal.expectedScheduleRevisionId,
        )
        requireCurrent(lifecycle, binding)
        if (
            remote.collectionRevision != target.collectionRevision ||
            !now().isBefore(Instant.parse(remote.expiresAt))
        ) throw InvalidSchedulePublicationResponseException()
        val previewed = journal.recordingPreview(remote)
        val previewSaved = plannerStore.replaceGoogleSchedulePublicationJournal(journal, previewed)
        requireCurrent(lifecycle, binding)
        if (!previewSaved) {
            throw SchedulePublicationRecoveryChangedException()
        }
        presentJournal(lifecycle, previewed)
        return GoogleSchedulePublicationOutcome.PREVIEW_READY
    }

    private suspend fun enqueueApproved(
        lifecycle: Long,
        binding: BoundSchedulePublicationConfiguration,
        journal: GoogleSchedulePublicationJournal,
    ): GoogleSchedulePublicationOutcome {
        require(journal.stage == GoogleSchedulePublicationStage.APPROVED)
        setState(
            lifecycle,
            GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.ENQUEUEING,
                "Saving the approved schedule to DayWeave’s durable Google outbox…",
                hasPendingRecovery = true,
                isBusy = true,
                configurationId = binding.configurationId,
            ),
        )
        requireCurrent(lifecycle, binding)
        val accepted = transport.enqueueSchedulePublication(
            binding.configuration,
            journal.accountId,
            requireNotNull(journal.preview).id,
            journal.collectionId,
            journal.expectedScheduleRevisionId,
            requireNotNull(journal.approvalCapability).value,
        )
        requireCurrent(lifecycle, binding)
        val recorded = journal.recordingAcceptance(accepted)
        val acceptanceSaved = plannerStore.replaceGoogleSchedulePublicationJournal(journal, recorded)
        requireCurrent(lifecycle, binding)
        if (!acceptanceSaved) {
            throw SchedulePublicationRecoveryChangedException()
        }
        return refreshAcceptedStatus(lifecycle, binding, recorded)
    }

    private suspend fun refreshAcceptedStatus(
        lifecycle: Long,
        binding: BoundSchedulePublicationConfiguration,
        journal: GoogleSchedulePublicationJournal,
    ): GoogleSchedulePublicationOutcome {
        require(journal.stage == GoogleSchedulePublicationStage.ACCEPTED)
        setState(
            lifecycle,
            GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.CHECKING_STATUS,
                "Checking Google Calendar delivery…",
                status = journal.status,
                hasPendingRecovery = true,
                isBusy = true,
                configurationId = binding.configurationId,
            ),
        )
        requireCurrent(lifecycle, binding)
        val remote = transport.schedulePublicationStatus(
            binding.configuration,
            journal.accountId,
            requireNotNull(journal.accepted).publicationId,
        )
        requireCurrent(lifecycle, binding)
        val updated = journal.recordingStatus(remote)
        val statusSaved = plannerStore.replaceGoogleSchedulePublicationJournal(journal, updated)
        requireCurrent(lifecycle, binding)
        if (!statusSaved) {
            throw SchedulePublicationRecoveryChangedException()
        }
        presentJournal(lifecycle, updated)
        return if (updated.status?.isTerminal == true) {
            GoogleSchedulePublicationOutcome.STATUS_UPDATED
        } else {
            GoogleSchedulePublicationOutcome.PENDING
        }
    }

    private fun presentJournal(lifecycle: Long, journal: GoogleSchedulePublicationJournal) {
        val currentTime = now()
        val state = when {
            journal.stage == GoogleSchedulePublicationStage.PREVIEWED &&
                currentTime.isBefore(Instant.parse(requireNotNull(journal.preview).expiresAt)) ->
                GoogleSchedulePublicationState(
                    GoogleSchedulePublicationPhase.AWAITING_APPROVAL,
                    "Review every Calendar change below, then approve this exact batch.",
                    preview = journal.preview,
                    hasPendingRecovery = true,
                    configurationId = journal.configurationId,
                )
            journal.stage == GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED &&
                !journal.canDiscardExpiredAt(currentTime) -> GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.RESPONSE_UNKNOWN,
                "The one-time approval response is unknown. It will not be retried.",
                hasPendingRecovery = true,
                configurationId = journal.configurationId,
            )
            journal.stage == GoogleSchedulePublicationStage.APPROVED ->
                GoogleSchedulePublicationState(
                    GoogleSchedulePublicationPhase.APPROVED_REPLAY_REQUIRED,
                    "The exact approved enqueue is saved. Confirm replay before sending it.",
                    hasPendingRecovery = true,
                    configurationId = journal.configurationId,
                )
            journal.stage == GoogleSchedulePublicationStage.ACCEPTED ->
                stateFromStatus(journal)
            journal.canDiscardExpiredAt(currentTime) -> GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.EXPIRED,
                "The saved approval authority expired and can now be discarded safely.",
                hasPendingRecovery = true,
                configurationId = journal.configurationId,
            )
            else -> GoogleSchedulePublicationState(
                GoogleSchedulePublicationPhase.RECOVERY_REQUIRED,
                "Recover this saved schedule publication before starting another.",
                hasPendingRecovery = true,
                configurationId = journal.configurationId,
            )
        }
        setState(lifecycle, state)
    }

    private fun stateFromStatus(journal: GoogleSchedulePublicationJournal): GoogleSchedulePublicationState {
        val status = journal.status
        val phase = when (status?.state) {
            null, ScheduleGooglePublicationState.PENDING,
            ScheduleGooglePublicationState.DELIVERING,
            ScheduleGooglePublicationState.BACKOFF,
            -> GoogleSchedulePublicationPhase.PENDING
            ScheduleGooglePublicationState.PARTIALLY_PUBLISHED ->
                GoogleSchedulePublicationPhase.PARTIALLY_PUBLISHED
            ScheduleGooglePublicationState.PUBLISHED -> GoogleSchedulePublicationPhase.PUBLISHED
            ScheduleGooglePublicationState.CONFLICT -> GoogleSchedulePublicationPhase.CONFLICT
            ScheduleGooglePublicationState.FAILED -> GoogleSchedulePublicationPhase.FAILED
            ScheduleGooglePublicationState.SUPERSEDED -> GoogleSchedulePublicationPhase.SUPERSEDED
        }
        val message = when (phase) {
            GoogleSchedulePublicationPhase.PENDING ->
                "Schedule publication is queued or delivering. You can check again safely."
            GoogleSchedulePublicationPhase.PARTIALLY_PUBLISHED ->
                "Some Calendar changes published; others need reconciliation."
            GoogleSchedulePublicationPhase.PUBLISHED ->
                "The generated schedule was published to Google Calendar."
            GoogleSchedulePublicationPhase.CONFLICT ->
                "Google or the generated schedule changed before every update could publish."
            GoogleSchedulePublicationPhase.FAILED ->
                "The publication stopped after a non-retryable delivery failure."
            GoogleSchedulePublicationPhase.SUPERSEDED ->
                "A newer generated schedule superseded this publication."
            else -> "Schedule publication status is available."
        }
        return GoogleSchedulePublicationState(
            phase,
            message,
            status = status,
            hasPendingRecovery = true,
            acceptedWasReplay = journal.accepted?.replayed,
            configurationId = journal.configurationId,
        )
    }

    private fun sourceAndTargetRemainCurrent(journal: GoogleSchedulePublicationJournal): Boolean =
        currentSourceAndTarget(journal)?.second?.collectionRevision == journal.preview?.collectionRevision

    private fun currentSourceAndTarget(
        journal: GoogleSchedulePublicationJournal,
    ): Pair<DayWeaveUiState, GoogleSchedulePublicationTarget>? {
        val planner = plannerStore.durableState.value ?: return null
        if (
            !planner.hasCurrentPublishedSchedule() ||
            planner.publishedScheduleRevision?.id != journal.expectedScheduleRevisionId
        ) return null
        val target = requireCurrentTarget(
            GoogleSchedulePublicationTarget(
                journal.accountId,
                journal.collectionId,
                journal.preview?.collectionRevision ?: pendingDestinationOption()?.target?.collectionRevision
                    ?: return null,
            ),
            journal.configurationId,
        ) ?: return null
        return planner to target
    }

    private fun requireCurrentTarget(
        requested: GoogleSchedulePublicationTarget,
        expectedConfigurationId: String,
    ): GoogleSchedulePublicationTarget? {
        val accounts = googleAccountState()
        val imports = googleImportState()
        if (
            accounts.phase != GoogleAccountPhase.CONNECTED || accounts.isBusy ||
            accounts.authorization != null || accounts.authorizationRecovery != null ||
            accounts.authorizationRecoveryResetRequired ||
            accounts.authorizationRecoveryDiscardRequired || imports.isBusy ||
            imports.pendingRecoveryCount != 0 ||
            accounts.configurationId != expectedConfigurationId ||
            imports.configurationId != expectedConfigurationId
        ) return null
        val account = accounts.accounts.singleOrNull { it.id == requested.accountId } ?: return null
        val collection = imports.accounts[requested.accountId]?.collections
            ?.singleOrNull { it.id == requested.collectionId } ?: return null
        return currentTarget(account, collection)?.takeIf { it == requested }
    }

    private fun currentTarget(
        account: GoogleAccountSummary,
        collection: GoogleImportCollectionState,
    ): GoogleSchedulePublicationTarget? = runCatching {
        require(account.status == "active" && account.syncEnabled && account.hasCalendarWriteScope)
        require(collection.accountId == account.id)
        require(collection.kind == RemoteGoogleCollectionKind.CALENDAR)
        require(collection.selected && !collection.providerDeleted)
        require(collection.syncRole == RemoteGoogleSyncRole.WRITABLE)
        require(collection.providerAccessRole?.lowercase(Locale.ROOT) in setOf("owner", "writer"))
        GoogleSchedulePublicationTarget(account.id, collection.id, collection.revision)
    }.getOrNull()

    private fun DayWeaveUiState.hasCurrentPublishedSchedule(): Boolean =
        pendingSchedulePublication == null && publishedScheduleRevision != null &&
            publishedScheduleProof?.matchesCurrentStateAndPlan(this) == true

    private suspend fun withBoundOperation(
        operation: suspend (
            lifecycle: Long,
            binding: BoundSchedulePublicationConfiguration,
        ) -> GoogleSchedulePublicationOutcome,
    ): GoogleSchedulePublicationOutcome {
        val lifecycle = lifecycleGeneration.get()
        val binding = authenticatedBinding(lifecycle) ?: return bindingFailureOutcome()
        val ticket = try {
            binding.configuration.beginBindingOperation()
        } catch (_: ApiBindingChangedException) {
            quarantineBindingState()
            return GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
        }
        return try {
            operation(lifecycle, binding)
        } catch (error: CancellationException) {
            retainRecoveryAfterFailure(lifecycle, binding, error)
            throw error
        } catch (_: ApiBindingChangedException) {
            quarantineBindingState()
            GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
        } catch (_: StaleSchedulePublicationOperationException) {
            GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
        } catch (error: Exception) {
            retainRecoveryAfterFailure(lifecycle, binding, error)
        } finally {
            ticket.release()
        }
    }

    private fun authenticatedBinding(lifecycle: Long): BoundSchedulePublicationConfiguration? {
        if (!operationAllowed()) return null
        val snapshot = credentialStore.snapshot()
        if (!snapshot.hasBearerToken || snapshot.configurationId == null || snapshot.baseUrl == null) {
            setState(lifecycle, initialState())
            return null
        }
        val configuration = try {
            credentialStore.authenticatedConfiguration()
        } catch (_: RuntimeException) {
            null
        } ?: return null
        if (
            configuration.configurationId != snapshot.configurationId ||
            configuration.baseUrl.toString() != snapshot.baseUrl
        ) return null
        return BoundSchedulePublicationConfiguration(
            snapshot,
            configuration,
            snapshot.configurationId,
            snapshot.baseUrl,
        )
    }

    private fun requireCurrent(
        lifecycle: Long,
        binding: BoundSchedulePublicationConfiguration,
    ) {
        if (
            !operationAllowed() || lifecycleGeneration.get() != lifecycle ||
            !sameBinding(credentialStore.snapshot(), binding.snapshot)
        ) throw StaleSchedulePublicationOperationException()
    }

    private fun setState(lifecycle: Long, next: GoogleSchedulePublicationState) {
        synchronized(presentationMonitor) {
            if (operationAllowed() && lifecycleGeneration.get() == lifecycle) mutableState.value = next
        }
    }

    private fun failure(
        lifecycle: Long,
        phase: GoogleSchedulePublicationPhase,
        message: String,
        outcome: GoogleSchedulePublicationOutcome,
    ): GoogleSchedulePublicationOutcome {
        setState(
            lifecycle,
            GoogleSchedulePublicationState(
                phase,
                message,
                hasPendingRecovery = plannerStore.state.value.pendingGoogleSchedulePublication != null,
                configurationId = credentialStore.snapshot().configurationId,
            ),
        )
        return outcome
    }

    private fun retainRecoveryAfterFailure(
        lifecycle: Long,
        binding: BoundSchedulePublicationConfiguration,
        error: Exception,
    ): GoogleSchedulePublicationOutcome {
        val journal = plannerStore.durableState.value?.pendingGoogleSchedulePublication
        if (journal?.stage == GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED) {
            presentJournal(lifecycle, journal)
            return GoogleSchedulePublicationOutcome.PENDING
        }
        val authentication = error is GoogleCalendarOutboundApiException.Authentication
        val stale = error is GoogleCalendarOutboundApiException.Conflict ||
            error is GoogleCalendarOutboundApiException.NotFound ||
            error is GoogleCalendarOutboundApiException.Validation ||
            error is GoogleCalendarOutboundApiException.InvalidResponse ||
            error is InvalidSchedulePublicationResponseException ||
            error is SchedulePublicationRecoveryChangedException
        val phase = when {
            authentication -> GoogleSchedulePublicationPhase.AUTH_REQUIRED
            stale -> GoogleSchedulePublicationPhase.RECOVERY_REQUIRED
            error is IOException -> GoogleSchedulePublicationPhase.OFFLINE
            else -> GoogleSchedulePublicationPhase.RECOVERY_REQUIRED
        }
        val message = when {
            authentication -> "DayWeave API authentication is required to recover publication."
            stale -> "The saved schedule or approval is stale. Recovery remains encrypted."
            error is IOException -> "Offline or unavailable · exact publication recovery remains saved."
            else -> "Publication could not be verified. Recovery remains saved."
        }
        setState(
            lifecycle,
            GoogleSchedulePublicationState(
                phase,
                message,
                status = journal?.status,
                hasPendingRecovery = journal != null,
                configurationId = binding.configurationId,
            ),
        )
        return when {
            authentication -> GoogleSchedulePublicationOutcome.AUTH_REQUIRED
            stale -> GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED
            else -> GoogleSchedulePublicationOutcome.PENDING
        }
    }

    private fun bindingFailureOutcome(): GoogleSchedulePublicationOutcome {
        plannerStore.durableState.value?.pendingGoogleSchedulePublication?.let {
            presentJournal(lifecycleGeneration.get(), it)
        }
        return if (credentialStore.snapshot().hasBearerToken) {
            GoogleSchedulePublicationOutcome.AUTH_REQUIRED
        } else {
            GoogleSchedulePublicationOutcome.NOT_CONFIGURED
        }
    }

    private fun readyState(binding: BoundSchedulePublicationConfiguration) =
        GoogleSchedulePublicationState(
            GoogleSchedulePublicationPhase.READY,
            "Choose a writable Google Calendar to publish the current generated schedule.",
            configurationId = binding.configurationId,
        )

    private fun initialState(): GoogleSchedulePublicationState = when {
        !operationAllowed() -> GoogleSchedulePublicationState(
            GoogleSchedulePublicationPhase.PRIVACY_PROTECTED,
            "Schedule publication details are hidden while DayWeave is locked.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleSchedulePublication != null,
        )
        !credentialStore.snapshot().hasBearerToken -> GoogleSchedulePublicationState(
            GoogleSchedulePublicationPhase.NOT_CONFIGURED,
            "Connect the DayWeave API before publishing a generated schedule.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleSchedulePublication != null,
        )
        else -> GoogleSchedulePublicationState(
            GoogleSchedulePublicationPhase.READY,
            "Choose a writable Google Calendar to publish the current generated schedule.",
            hasPendingRecovery = plannerStore.state.value.pendingGoogleSchedulePublication != null,
            configurationId = credentialStore.snapshot().configurationId,
        )
    }

    private data class BoundSchedulePublicationConfiguration(
        val snapshot: ApiConnectionSnapshot,
        val configuration: AuthenticatedApiConfiguration,
        val configurationId: String,
        val apiBaseUrl: String,
    )

    private class StaleSchedulePublicationOperationException : IOException()
    private class InvalidSchedulePublicationResponseException : IOException()
    private class SchedulePublicationRecoveryChangedException : IOException()

    private companion object {
        fun sameBinding(left: ApiConnectionSnapshot, right: ApiConnectionSnapshot): Boolean =
            left.baseUrl == right.baseUrl && left.hasBearerToken == right.hasBearerToken &&
                left.configurationId == right.configurationId
    }
}
