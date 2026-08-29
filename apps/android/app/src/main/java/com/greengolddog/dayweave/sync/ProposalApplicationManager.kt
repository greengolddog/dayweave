package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DAYWEAVE_PROPOSAL_CHANGE_SET_SCHEMA_V1
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.isApplicationReady
import com.greengolddog.dayweave.model.usesReservedChangeSetNamespace
import com.greengolddog.dayweave.network.ApiBindingChangedException
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.InvalidApiConfigurationException
import com.greengolddog.dayweave.network.ProposalApplicationApiException
import com.greengolddog.dayweave.network.ProposalApplicationsTransport
import com.greengolddog.dayweave.network.ProposalPreviewMember
import com.greengolddog.dayweave.network.ProposalPreviewRequest
import com.greengolddog.dayweave.network.RemoteProposalApplicationPreview
import com.greengolddog.dayweave.network.RemoteProposalApplicationReceipt
import com.greengolddog.dayweave.network.RemoteProposalApplicationStatus
import com.greengolddog.dayweave.network.RemoteProposalConflictCode
import com.greengolddog.dayweave.network.SecureCredentialException
import com.greengolddog.dayweave.network.prepareProposalApplyHttpRequest
import com.greengolddog.dayweave.network.prepareProposalUndoHttpRequest
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

enum class ProposalApplicationPhase {
    IDLE,
    NOT_CONFIGURED,
    AUTH_REQUIRED,
    PREVIEWING,
    REVIEW_READY,
    APPLYING,
    UNDOING,
    RECOVERING,
    COMPLETED,
    ERROR,
}

data class ProposalApplicationState(
    val phase: ProposalApplicationPhase = ProposalApplicationPhase.IDLE,
    val message: String = "Review a typed proposal before it can change canonical items.",
    val activeProposalId: String? = null,
    val preview: RemoteProposalApplicationPreview? = null,
    val receipt: ProposalApplicationReceiptSnapshot? = null,
) {
    val isBusy: Boolean
        get() = phase in setOf(
            ProposalApplicationPhase.PREVIEWING,
            ProposalApplicationPhase.APPLYING,
            ProposalApplicationPhase.UNDOING,
            ProposalApplicationPhase.RECOVERING,
        )

    val exactApproval: ProposalApplicationApproval?
        get() {
            val reviewed = preview ?: return null
            val member = reviewed.proposals.singleOrNull() ?: return null
            if (activeProposalId != member.proposalId) return null
            return ProposalApplicationApproval(
                proposalId = member.proposalId,
                expectedProposalRevision = member.expectedRevision,
                previewId = reviewed.previewId,
                reviewHash = reviewed.reviewHash,
            )
        }
}

/** User approval bound to one immutable server review and proposal revision. */
data class ProposalApplicationApproval(
    val proposalId: String,
    val expectedProposalRevision: Long,
    val previewId: String,
    val reviewHash: String,
)

/**
 * Single-proposal transactional review/apply/undo coordinator.
 *
 * Review contents remain memory-only. Only the exact non-secret request envelope and a
 * content-free receipt cross the encrypted persistence boundary.
 */
class ProposalApplicationManager(
    private val plannerStore: PlannerStore,
    private val credentialStore: ApiCredentialStore,
    private val transport: ProposalApplicationsTransport,
    private val nowEpochMillis: () -> Long = System::currentTimeMillis,
) {
    private val operationMutex = Mutex()
    private val reviewPrivacyGeneration = AtomicLong()
    private val mutableState = MutableStateFlow(initialState())
    val state: StateFlow<ProposalApplicationState> = mutableState.asStateFlow()

    internal fun quarantineBindingState() {
        reviewPrivacyGeneration.incrementAndGet()
        mutableState.value = initialState()
    }

    fun discardReview(proposalId: String? = null) {
        val current = mutableState.value
        if (
            !current.isBusy &&
            (proposalId == null || current.activeProposalId == proposalId)
        ) {
            reviewPrivacyGeneration.incrementAndGet()
            mutableState.value = initialState()
        }
    }

    /** Drops all ephemeral review content without disturbing an exact journaled mutation. */
    fun discardReviewForPrivacyBoundary() {
        reviewPrivacyGeneration.incrementAndGet()
        val current = mutableState.value
        mutableState.value = when {
            current.preview == null && current.phase != ProposalApplicationPhase.PREVIEWING -> current
            current.phase == ProposalApplicationPhase.APPLYING -> current.copy(preview = null)
            else -> initialState()
        }
    }

    suspend fun prepareReview(proposalId: String): Boolean {
        if (!awaitReadyPlanner()) return false
        return operationMutex.withLock {
            if (plannerStore.state.value.pendingProposalApplicationMutation != null) {
                fail("Recover the interrupted proposal operation before starting another review.")
                return@withLock false
            }
            val proposal = actionableProposal(proposalId) ?: return@withLock false
            val configuration = authenticatedConfiguration() ?: return@withLock false
            val privacyGeneration = reviewPrivacyGeneration.get()
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.PREVIEWING,
                message = "Simulating the exact proposal without changing canonical items…",
                activeProposalId = proposal.id,
            )
            try {
                configuration.withBindingOperation {
                    val preview = transport.preview(
                        configuration,
                        ProposalPreviewRequest(
                            proposals = listOf(
                                ProposalPreviewMember(
                                    proposalId = proposal.id,
                                    expectedRevision = requireNotNull(proposal.remoteRevision),
                                ),
                            ),
                        ),
                    )
                    validateReview(preview, proposal)
                    requireLatestProposal(proposal)
                    if (reviewPrivacyGeneration.get() != privacyGeneration) {
                        mutableState.value = initialState()
                        return@withBindingOperation false
                    }
                    mutableState.value = ProposalApplicationState(
                        phase = ProposalApplicationPhase.REVIEW_READY,
                        message = if (preview.canApply) {
                            "Review complete. Explicit confirmation is required to apply these changes."
                        } else {
                            "Review complete. Conflicts block this proposal until it is refreshed or revised."
                        },
                        activeProposalId = proposal.id,
                        preview = preview,
                    )
                    true
                }
            } catch (error: Throwable) {
                handleFailure(error, "The proposal review could not be loaded.")
                false
            }
        }
    }

    suspend fun applyReviewed(approval: ProposalApplicationApproval): Boolean {
        if (!awaitReadyPlanner()) return false
        return operationMutex.withLock {
            if (plannerStore.state.value.pendingProposalApplicationMutation != null) {
                fail("Recover the interrupted proposal operation before applying another proposal.")
                return@withLock false
            }
            val proposal = actionableProposal(approval.proposalId) ?: return@withLock false
            val reviewState = mutableState.value
            val preview = reviewState.preview?.takeIf {
                reviewState.activeProposalId == proposal.id
            } ?: run {
                fail("This review is unavailable. Generate a fresh exact review before applying.")
                return@withLock false
            }
            val expectedApproval = reviewState.exactApproval
            if (
                expectedApproval == null ||
                approval != expectedApproval ||
                approval.expectedProposalRevision != proposal.remoteRevision
            ) {
                fail("This approval belongs to a different or stale review. Generate a fresh exact review.")
                return@withLock false
            }
            try {
                validateReview(preview, proposal)
            } catch (error: Throwable) {
                handleFailure(error, "This review is stale. Generate a fresh review before applying.")
                return@withLock false
            }
            if (!preview.canApply || preview.conflicts.isNotEmpty()) {
                fail("The reviewed changes contain conflicts and cannot be applied.")
                return@withLock false
            }
            val configuration = authenticatedConfiguration() ?: return@withLock false
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.APPLYING,
                message = "Applying the exact reviewed changes atomically…",
                activeProposalId = proposal.id,
                preview = preview,
            )
            try {
                configuration.withBindingOperation {
                    requireLatestProposal(proposal)
                    val request = prepareProposalApplyHttpRequest(
                        configuration = configuration,
                        previewId = preview.previewId,
                        expectedReviewHash = preview.reviewHash,
                    )
                    val pending = PendingProposalApplicationMutation(
                        schemaVersion = JOURNAL_VERSION,
                        kind = ProposalApplicationMutationKind.APPLY,
                        idempotencyKey = UUID.randomUUID().toString(),
                        syncOrigin = configuration.baseUrl.toString(),
                        configurationId = configuration.configurationId,
                        proposalId = proposal.id,
                        expectedProposalRevision = requireNotNull(proposal.remoteRevision),
                        expectedCommandIds = preview.commandIds,
                        previewId = preview.previewId,
                        expectedReviewHash = preview.reviewHash,
                        preparedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
                        request = request,
                    )
                    val staged = plannerStore.stageProposalApplicationMutation(pending)
                    if (staged == null || !staged.awaitDurable()) {
                        fail("The exact request could not be saved; nothing was sent.")
                        return@withBindingOperation false
                    }
                    try {
                        val response = transport.apply(
                            configuration = configuration,
                            previewId = preview.previewId,
                            expectedReviewHash = preview.reviewHash,
                            idempotencyKey = pending.idempotencyKey,
                            request = pending.request,
                        )
                        finishPending(
                            pending = pending,
                            remote = response.application,
                            allowAlreadyUndone = false,
                        )
                    } catch (error: Throwable) {
                        if (error is CancellationException) throw error
                        recoverApply(configuration, pending)
                    }
                }
            } catch (error: Throwable) {
                handleFailure(error, "The apply outcome remains unresolved; the exact request was retained.")
                false
            }
        }
    }

    suspend fun undo(proposalId: String): Boolean {
        if (!awaitReadyPlanner()) return false
        return operationMutex.withLock {
            if (plannerStore.state.value.pendingProposalApplicationMutation != null) {
                fail("Recover the interrupted proposal operation before starting an undo.")
                return@withLock false
            }
            val previous = plannerStore.state.value.proposalApplications[proposalId]
                ?: run {
                    fail("The durable application receipt is unavailable.")
                    return@withLock false
                }
            if (
                previous.status != ProposalApplicationStatusSnapshot.APPLIED ||
                Instant.parse(previous.undoExpiresAt).toEpochMilli() <= nowEpochMillis()
            ) {
                fail("The bounded undo window has expired or this application is already undone.")
                return@withLock false
            }
            val configuration = authenticatedConfiguration() ?: return@withLock false
            if (!previous.matches(configuration)) {
                fail("This receipt belongs to another authenticated API configuration.")
                return@withLock false
            }
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.UNDOING,
                message = "Undoing the proposal application atomically…",
                activeProposalId = proposalId,
                receipt = previous,
            )
            try {
                configuration.withBindingOperation {
                    val request = prepareProposalUndoHttpRequest(
                        configuration = configuration,
                        applicationId = previous.applicationId,
                        expectedApplicationRevision = previous.applicationRevision,
                    )
                    val pending = PendingProposalApplicationMutation(
                        schemaVersion = JOURNAL_VERSION,
                        kind = ProposalApplicationMutationKind.UNDO,
                        idempotencyKey = UUID.randomUUID().toString(),
                        syncOrigin = configuration.baseUrl.toString(),
                        configurationId = configuration.configurationId,
                        proposalId = previous.proposalId,
                        expectedProposalRevision = previous.appliedProposalRevision,
                        expectedCommandIds = previous.commandIds,
                        applicationId = previous.applicationId,
                        expectedApplicationRevision = previous.applicationRevision,
                        preparedAt = Instant.ofEpochMilli(nowEpochMillis()).toString(),
                        request = request,
                    )
                    val staged = plannerStore.stageProposalApplicationMutation(pending)
                    if (staged == null || !staged.awaitDurable()) {
                        fail("The exact undo request could not be saved; nothing was sent.")
                        return@withBindingOperation false
                    }
                    try {
                        val response = transport.undo(
                            configuration = configuration,
                            applicationId = previous.applicationId,
                            expectedApplicationRevision = previous.applicationRevision,
                            idempotencyKey = pending.idempotencyKey,
                            request = pending.request,
                        )
                        finishUndo(pending, previous, response.application)
                    } catch (error: Throwable) {
                        if (error is CancellationException) throw error
                        recoverUndo(configuration, pending, previous)
                    }
                }
            } catch (error: Throwable) {
                handleFailure(error, "The undo outcome remains unresolved; the exact request was retained.")
                false
            }
        }
    }

    /** Recovers a process-death or lost-response journal before canonical state may refresh. */
    suspend fun recoverPending(): Boolean {
        if (!awaitReadyPlanner()) return false
        return operationMutex.withLock {
            val pending = plannerStore.state.value.pendingProposalApplicationMutation
                ?: return@withLock true
            try {
                plannerStore.validateProposalApplicationMutation(pending)
            } catch (error: Throwable) {
                fail("The encrypted proposal recovery record is invalid. No request was sent.")
                return@withLock false
            }
            val configuration = authenticatedConfiguration() ?: return@withLock false
            if (!pending.matches(configuration)) {
                fail("The pending operation belongs to another authenticated API configuration.")
                return@withLock false
            }
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.RECOVERING,
                message = "Recovering the outcome of an interrupted proposal operation…",
                activeProposalId = pending.proposalId,
            )
            try {
                configuration.withBindingOperation {
                    when (pending.kind) {
                        ProposalApplicationMutationKind.APPLY ->
                            recoverApply(configuration, pending)
                        ProposalApplicationMutationKind.UNDO -> {
                            val previous = plannerStore.state.value
                                .proposalApplications[pending.proposalId]
                            if (
                                previous == null ||
                                previous.applicationId != pending.applicationId ||
                                previous.applicationRevision != pending.expectedApplicationRevision ||
                                previous.status != ProposalApplicationStatusSnapshot.APPLIED
                            ) {
                                fail("The applied receipt required to recover this undo is unavailable.")
                                false
                            } else {
                                recoverUndo(configuration, pending, previous)
                            }
                        }
                    }
                }
            } catch (error: Throwable) {
                handleFailure(error, "The proposal operation is still unresolved; its exact request was retained.")
                false
            }
        }
    }

    private suspend fun recoverApply(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingProposalApplicationMutation,
    ): Boolean {
        mutableState.value = ProposalApplicationState(
            phase = ProposalApplicationPhase.RECOVERING,
            message = "Checking the durable application result before any exact replay…",
            activeProposalId = pending.proposalId,
        )
        try {
            val existing = transport.getByProposal(configuration, pending.proposalId)
            return finishPending(pending, existing, allowAlreadyUndone = true)
        } catch (error: Throwable) {
            if (error !is ProposalApplicationApiException.NotFound) {
                if (error is CancellationException) throw error
                handleFailure(
                    error,
                    "The apply outcome is still unresolved; the exact request remains saved.",
                )
                return false
            }
        }

        return try {
            val response = transport.apply(
                configuration = configuration,
                previewId = requireNotNull(pending.previewId),
                expectedReviewHash = requireNotNull(pending.expectedReviewHash),
                idempotencyKey = pending.idempotencyKey,
                request = pending.request,
            )
            finishPending(pending, response.application, allowAlreadyUndone = false)
        } catch (error: Throwable) {
            if (error is CancellationException) throw error
            if (isDefinitiveApplyNoMutation(error)) {
                val cleared = plannerStore.clearPendingProposalApplicationMutation(
                    pending,
                    "The reviewed proposal was not applied; generate a fresh review.",
                )
                if (cleared != null && cleared.awaitDurable()) {
                    fail("The reviewed proposal was not applied. Generate a fresh review.")
                    return false
                }
            }
            handleFailure(
                error,
                "The apply outcome is still unresolved; the exact request remains saved.",
            )
            false
        }
    }

    private suspend fun recoverUndo(
        configuration: AuthenticatedApiConfiguration,
        pending: PendingProposalApplicationMutation,
        previous: ProposalApplicationReceiptSnapshot,
    ): Boolean {
        mutableState.value = ProposalApplicationState(
            phase = ProposalApplicationPhase.RECOVERING,
            message = "Checking the durable undo result before any exact replay…",
            activeProposalId = pending.proposalId,
            receipt = previous,
        )
        var lookupWasNotFound = false
        try {
            val existing = transport.getById(configuration, previous.applicationId)
            if (existing.status == RemoteProposalApplicationStatus.UNDONE) {
                return finishUndo(pending, previous, existing)
            }
            validateRemoteMatchesStoredApplied(existing, previous)
        } catch (error: Throwable) {
            if (error is CancellationException) throw error
            lookupWasNotFound = error is ProposalApplicationApiException.NotFound
            if (!lookupWasNotFound) {
                handleFailure(
                    error,
                    "The undo outcome is still unresolved; the exact request remains saved.",
                )
                return false
            }
        }

        return try {
            val response = transport.undo(
                configuration = configuration,
                applicationId = previous.applicationId,
                expectedApplicationRevision = previous.applicationRevision,
                idempotencyKey = pending.idempotencyKey,
                request = pending.request,
            )
            finishUndo(pending, previous, response.application)
        } catch (error: Throwable) {
            if (error is CancellationException) throw error
            if (isDefinitiveUndoNoMutation(error)) {
                val cleared = plannerStore.clearPendingProposalApplicationMutation(
                    pending,
                    "The undo was not performed; the applied receipt was retained.",
                )
                if (cleared != null && cleared.awaitDurable()) {
                    fail("The undo was not performed; the applied receipt was retained.")
                    return false
                }
            }
            handleFailure(
                error,
                "The undo outcome is still unresolved; the exact request remains saved.",
            )
            false
        }
    }

    private suspend fun finishPending(
        pending: PendingProposalApplicationMutation,
        remote: RemoteProposalApplicationReceipt,
        allowAlreadyUndone: Boolean,
    ): Boolean {
        require(pending.kind == ProposalApplicationMutationKind.APPLY)
        val member = remote.proposals.single()
        require(member.proposalId == pending.proposalId)
        require(member.appliedRevision == Math.addExact(pending.expectedProposalRevision, 1L))
        require(remote.commandIds == pending.expectedCommandIds)
        require(
            remote.status == RemoteProposalApplicationStatus.APPLIED ||
                allowAlreadyUndone && remote.status == RemoteProposalApplicationStatus.UNDONE,
        )
        return commitPending(pending, remote)
    }

    private suspend fun finishUndo(
        pending: PendingProposalApplicationMutation,
        previous: ProposalApplicationReceiptSnapshot,
        remote: RemoteProposalApplicationReceipt,
    ): Boolean {
        require(pending.kind == ProposalApplicationMutationKind.UNDO)
        val member = remote.proposals.single()
        require(remote.status == RemoteProposalApplicationStatus.UNDONE)
        require(remote.applicationId == previous.applicationId)
        require(remote.applicationRevision == Math.addExact(previous.applicationRevision, 1L))
        require(member.proposalId == previous.proposalId)
        require(member.appliedRevision == previous.appliedProposalRevision)
        require(remote.commandIds == previous.commandIds)
        require(remote.affectedItemIds == previous.affectedItemIds)
        require(remote.appliedAt == previous.appliedAt)
        require(remote.undoExpiresAt == previous.undoExpiresAt)
        return commitPending(pending, remote)
    }

    private suspend fun commitPending(
        pending: PendingProposalApplicationMutation,
        remote: RemoteProposalApplicationReceipt,
    ): Boolean {
        val receipt = remote.toSnapshot(pending)
        val committed = plannerStore.commitProposalApplicationMutation(pending, receipt)
        if (committed == null || !committed.awaitDurable()) {
            fail("The server result could not be stored; exact recovery remains required.")
            return false
        }
        mutableState.value = ProposalApplicationState(
            phase = ProposalApplicationPhase.COMPLETED,
            message = if (receipt.status == ProposalApplicationStatusSnapshot.UNDONE) {
                "Proposal application undone. Refreshing canonical items and schedule…"
            } else {
                "Proposal applied transactionally. Refreshing canonical items and schedule…"
            },
            activeProposalId = pending.proposalId,
            receipt = receipt,
        )
        return true
    }

    private fun validateRemoteMatchesStoredApplied(
        remote: RemoteProposalApplicationReceipt,
        stored: ProposalApplicationReceiptSnapshot,
    ) {
        val member = remote.proposals.single()
        require(remote.status == RemoteProposalApplicationStatus.APPLIED)
        require(remote.applicationId == stored.applicationId)
        require(remote.applicationRevision == stored.applicationRevision)
        require(member.proposalId == stored.proposalId)
        require(member.appliedRevision == stored.appliedProposalRevision)
        require(remote.commandIds == stored.commandIds)
        require(remote.affectedItemIds == stored.affectedItemIds)
        require(remote.appliedAt == stored.appliedAt)
        require(remote.undoExpiresAt == stored.undoExpiresAt)
        require(remote.undoneAt == null)
    }

    private fun RemoteProposalApplicationReceipt.toSnapshot(
        pending: PendingProposalApplicationMutation,
    ): ProposalApplicationReceiptSnapshot {
        val member = proposals.single()
        return ProposalApplicationReceiptSnapshot(
            schemaVersion = RECEIPT_VERSION,
            syncOrigin = pending.syncOrigin,
            configurationId = pending.configurationId,
            applicationId = applicationId,
            proposalId = member.proposalId,
            appliedProposalRevision = member.appliedRevision,
            applicationRevision = applicationRevision,
            status = when (status) {
                RemoteProposalApplicationStatus.APPLIED -> ProposalApplicationStatusSnapshot.APPLIED
                RemoteProposalApplicationStatus.UNDONE -> ProposalApplicationStatusSnapshot.UNDONE
            },
            commandIds = commandIds,
            affectedItemIds = affectedItemIds,
            appliedAt = appliedAt,
            undoExpiresAt = undoExpiresAt,
            undoneAt = undoneAt,
        )
    }

    private fun validateReview(
        preview: RemoteProposalApplicationPreview,
        proposal: PlanningSuggestion,
    ) {
        require(preview.changeSetSchema == DAYWEAVE_PROPOSAL_CHANGE_SET_SCHEMA_V1)
        require(preview.proposals == listOf(
            ProposalPreviewMember(
                proposalId = proposal.id,
                expectedRevision = requireNotNull(proposal.remoteRevision),
            ),
        ))
        val expiresAt = Instant.parse(preview.expiresAt)
        require(expiresAt.toEpochMilli() > nowEpochMillis())
        proposal.remoteExpiresAt?.let { proposalExpiry ->
            require(expiresAt <= Instant.parse(proposalExpiry))
        }
        require(preview.canApply == preview.conflicts.isEmpty())
        require(!preview.canApply || preview.diffs.size == preview.commandIds.size)
    }

    private fun requireLatestProposal(expected: PlanningSuggestion) {
        val latest = plannerStore.state.value.suggestions.firstOrNull { it.id == expected.id }
        require(
            latest != null && latest.disposition == SuggestionDisposition.PENDING &&
                latest.remoteRevision == expected.remoteRevision &&
                latest.remotePayloadSchema == expected.remotePayloadSchema,
        ) { "The proposal changed after this review started" }
    }

    private fun actionableProposal(proposalId: String): PlanningSuggestion? {
        val proposal = plannerStore.state.value.suggestions.firstOrNull { it.id == proposalId }
        if (proposal == null || proposal.disposition != SuggestionDisposition.PENDING) {
            fail("This proposal is no longer pending. Refresh the Suggestions Inbox.")
            return null
        }
        if (!proposal.isApplicationReady) {
            fail(
                if (proposal.usesReservedChangeSetNamespace) {
                    "This proposal uses a newer protected change-set format. Update DayWeave before applying it."
                } else {
                    "This advisory proposal can only be accepted as a reviewable Inbox draft."
                },
            )
            return null
        }
        val expiresAt = proposal.remoteExpiresAt?.let { runCatching { Instant.parse(it) }.getOrNull() }
        if (expiresAt == null || expiresAt.toEpochMilli() <= nowEpochMillis()) {
            fail("This proposal has expired. Refresh the Suggestions Inbox.")
            return null
        }
        if (requireNotNull(proposal.remoteRevision) == Long.MAX_VALUE) {
            fail("This proposal revision cannot be applied safely.")
            return null
        }
        return proposal
    }

    private suspend fun awaitReadyPlanner(): Boolean {
        val load = plannerStore.loadState.first { it != PlannerLoadState.LOADING }
        if (load != PlannerLoadState.READY) {
            fail("Encrypted planner storage is unavailable; no proposal request was sent.")
            return false
        }
        return true
    }

    private fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        val snapshot = credentialStore.snapshot()
        if (snapshot.baseUrl == null) {
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.NOT_CONFIGURED,
                message = "Configure a durable device API session before reviewing typed changes.",
            )
            return null
        }
        if (!snapshot.hasBearerToken) {
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.AUTH_REQUIRED,
                message = "Restore the durable device session before reviewing typed changes.",
            )
            return null
        }
        return try {
            credentialStore.authenticatedConfiguration() ?: run {
                mutableState.value = ProposalApplicationState(
                    phase = ProposalApplicationPhase.AUTH_REQUIRED,
                    message = "Restore the durable device session before reviewing typed changes.",
                )
                null
            }
        } catch (_: SecureCredentialException) {
            mutableState.value = ProposalApplicationState(
                phase = ProposalApplicationPhase.AUTH_REQUIRED,
                message = "The encrypted device credential is unavailable. Reconnect this device.",
            )
            null
        } catch (_: InvalidApiConfigurationException) {
            fail("The stored API configuration is invalid.")
            null
        } catch (_: IllegalStateException) {
            fail("Secure API credentials are unavailable on this device.")
            null
        }
    }

    private fun PendingProposalApplicationMutation.matches(
        configuration: AuthenticatedApiConfiguration,
    ): Boolean = syncOrigin == configuration.baseUrl.toString() &&
        configurationId == configuration.configurationId

    private fun ProposalApplicationReceiptSnapshot.matches(
        configuration: AuthenticatedApiConfiguration,
    ): Boolean = syncOrigin == configuration.baseUrl.toString() &&
        configurationId == configuration.configurationId

    private fun handleFailure(error: Throwable, fallback: String) {
        if (error is CancellationException) {
            mutableState.value = initialState()
            throw error
        }
        val phase = when (error) {
            is ProposalApplicationApiException.Authentication ->
                ProposalApplicationPhase.AUTH_REQUIRED
            is ApiBindingChangedException -> if (credentialStore.snapshot().hasBearerToken) {
                ProposalApplicationPhase.ERROR
            } else {
                ProposalApplicationPhase.NOT_CONFIGURED
            }
            else -> ProposalApplicationPhase.ERROR
        }
        val message = when (error) {
            is ProposalApplicationApiException.Authentication ->
                "Authentication failed. Restore or replace the durable device session."
            is ProposalApplicationApiException.Authorization ->
                "This device is not authorized to review or apply proposals."
            is ProposalApplicationApiException.Conflict ->
                "The proposal or canonical items changed. Refresh and generate a new review."
            is ProposalApplicationApiException.Validation ->
                "The server rejected this exact reviewed request. Generate a fresh review."
            is ProposalApplicationApiException.InvalidRequest ->
                "The exact saved request failed local validation; nothing new was sent."
            is ProposalApplicationApiException.InvalidResponse ->
                "The server returned an incompatible proposal-application response."
            is ProposalApplicationApiException.Http ->
                "The DayWeave API returned HTTP ${error.statusCode}. The exact recovery record was kept."
            is ApiBindingChangedException ->
                "The authenticated API configuration changed; the old operation was not rebound."
            is IOException ->
                "Offline or unable to reach the API. The exact recovery record was kept."
            else -> fallback
        }
        mutableState.value = ProposalApplicationState(
            phase = phase,
            message = message,
            activeProposalId = plannerStore.state.value.pendingProposalApplicationMutation?.proposalId,
            receipt = plannerStore.state.value.pendingProposalApplicationMutation?.proposalId?.let {
                plannerStore.state.value.proposalApplications[it]
            },
        )
    }

    private fun fail(message: String) {
        mutableState.value = ProposalApplicationState(
            phase = ProposalApplicationPhase.ERROR,
            message = message,
            activeProposalId = plannerStore.state.value.pendingProposalApplicationMutation?.proposalId,
        )
    }

    private fun initialState(): ProposalApplicationState =
        if (plannerStore.state.value.pendingProposalApplicationMutation != null) {
            ProposalApplicationState(
                phase = ProposalApplicationPhase.IDLE,
                message = "An interrupted proposal operation is ready for safe recovery.",
                activeProposalId = plannerStore.state.value
                    .pendingProposalApplicationMutation?.proposalId,
            )
        } else {
            ProposalApplicationState()
        }

    private fun isDefinitiveApplyNoMutation(error: Throwable): Boolean =
        error is ProposalApplicationApiException.Conflict &&
            error.conflictCode in APPLY_NO_MUTATION_CONFLICTS

    private fun isDefinitiveUndoNoMutation(error: Throwable): Boolean =
        error is ProposalApplicationApiException.Conflict &&
            error.conflictCode in UNDO_NO_MUTATION_CONFLICTS

    private companion object {
        const val JOURNAL_VERSION = 1
        const val RECEIPT_VERSION = 1

        val APPLY_NO_MUTATION_CONFLICTS = setOf(
            RemoteProposalConflictCode.PROPOSAL_NOT_PENDING,
            RemoteProposalConflictCode.PROPOSAL_EXPIRED,
            RemoteProposalConflictCode.PROPOSAL_REVISION_MISMATCH,
            RemoteProposalConflictCode.ITEM_ALREADY_EXISTS,
            RemoteProposalConflictCode.ITEM_NOT_FOUND,
            RemoteProposalConflictCode.ITEM_REVISION_MISMATCH,
            RemoteProposalConflictCode.PARENT_NOT_FOUND,
            RemoteProposalConflictCode.HIERARCHY_CYCLE,
            RemoteProposalConflictCode.INVALID_PARENT_STATE,
            RemoteProposalConflictCode.NON_LEAF_EXECUTABLE,
            RemoteProposalConflictCode.HAS_CHILDREN,
            RemoteProposalConflictCode.DELETED_PARENT,
            RemoteProposalConflictCode.INVALID_ITEM,
            RemoteProposalConflictCode.PROVIDER_MANAGED_ITEM,
            RemoteProposalConflictCode.PREVIEW_EXPIRED,
            RemoteProposalConflictCode.PREVIEW_MISMATCH,
            RemoteProposalConflictCode.PREVIEW_NOT_APPLICABLE,
        )

        val UNDO_NO_MUTATION_CONFLICTS = setOf(
            RemoteProposalConflictCode.UNDO_EXPIRED,
            RemoteProposalConflictCode.UNDO_DIVERGED,
        )
    }
}
