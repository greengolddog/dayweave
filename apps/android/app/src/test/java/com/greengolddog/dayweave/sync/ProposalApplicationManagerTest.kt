package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PlanningSuggestion
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.SuggestionDisposition
import com.greengolddog.dayweave.model.SuggestionKind
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ProposalApplicationHttpRequest
import com.greengolddog.dayweave.network.ProposalApplicationApiException
import com.greengolddog.dayweave.network.ProposalApplicationsTransport
import com.greengolddog.dayweave.network.ProposalPreviewMember
import com.greengolddog.dayweave.network.ProposalPreviewRequest
import com.greengolddog.dayweave.network.RemoteProposalApplicationPreview
import com.greengolddog.dayweave.network.RemoteProposalApplicationReceipt
import com.greengolddog.dayweave.network.RemoteProposalApplicationStatus
import com.greengolddog.dayweave.network.RemoteProposalApplyResponse
import com.greengolddog.dayweave.network.RemoteProposalAppliedMember
import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalConflictCode
import com.greengolddog.dayweave.network.RemoteProposalItemDiff
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.network.RemoteProposalItemKind
import com.greengolddog.dayweave.network.RemoteProposalItemStatus
import com.greengolddog.dayweave.network.RemoteProposalOperation
import com.greengolddog.dayweave.network.RemoteProposalRiskLevel
import com.greengolddog.dayweave.network.RemoteProposalUndoResponse
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ProposalApplicationManagerTest {
    private val now = Instant.parse("2026-08-30T10:00:00Z").toEpochMilli()

    @Test
    fun exactReviewApplyReceiptAndUndoCompleteWithoutLegacyDraft() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        var requestObservedAfterDurableStage = false
        transport.onApply = {
            requestObservedAfterDurableStage =
                store.state.value.pendingProposalApplicationMutation != null
        }
        val manager = manager(store, transport)

        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertEquals(ProposalApplicationPhase.REVIEW_READY, manager.state.value.phase)
        assertTrue(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))

        assertTrue(requestObservedAfterDurableStage)
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            SuggestionDisposition.TRANSACTIONALLY_APPLIED,
            store.state.value.suggestions.single().disposition,
        )
        assertTrue(store.state.value.inbox.isEmpty())
        val applied = store.state.value.proposalApplications.getValue(PROPOSAL_ID)
        assertEquals(ProposalApplicationStatusSnapshot.APPLIED, applied.status)
        assertEquals(ProposalApplicationPhase.COMPLETED, manager.state.value.phase)

        assertTrue(manager.undo(PROPOSAL_ID))

        val undone = store.state.value.proposalApplications.getValue(PROPOSAL_ID)
        assertEquals(ProposalApplicationStatusSnapshot.UNDONE, undone.status)
        assertEquals(2L, undone.applicationRevision)
        assertNotNull(undone.undoneAt)
        assertEquals(1, transport.undoCalls)
    }

    @Test
    fun lostApplyResponseIsRecoveredByProposalBeforeAnyReplay() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport().apply {
            applyFailure = IOException("synthetic lost response")
            proposalLookup = appliedReceipt()
        }
        val manager = manager(store, transport)

        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertTrue(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))

        assertEquals(1, transport.applyCalls)
        assertEquals(1, transport.proposalLookupCalls)
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            ProposalApplicationStatusSnapshot.APPLIED,
            store.state.value.proposalApplications.getValue(PROPOSAL_ID).status,
        )
    }

    @Test
    fun transientLookupFailureRetainsExactApplyJournalForRestart() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport().apply {
            applyFailure = IOException("synthetic lost response")
            proposalLookupFailure = IOException("synthetic lookup outage")
        }
        val manager = manager(store, transport)

        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertFalse(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))

        val journal = requireNotNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(PROPOSAL_ID, journal.proposalId)
        assertEquals(REVIEW_HASH, journal.expectedReviewHash)
        assertEquals(listOf(COMMAND_ID), journal.expectedCommandIds)
        assertEquals(1, transport.applyCalls)
        assertEquals(1, transport.proposalLookupCalls)
        assertEquals(ProposalApplicationPhase.ERROR, manager.state.value.phase)

        transport.applyFailure = null
        transport.proposalLookupFailure = null
        val restarted = PlannerStore(store.state.value)
        val restartedManager = manager(restarted, transport)
        assertTrue(restartedManager.recoverPending())
        assertEquals(2, transport.applyCalls)
        assertEquals(2, transport.proposalLookupCalls)
        assertEquals(transport.applyRequests[0], transport.applyRequests[1])
        assertEquals(journal.request, transport.applyRequests[1])
        assertEquals(transport.applyIdempotencyKeys[0], transport.applyIdempotencyKeys[1])
        assertEquals(journal.idempotencyKey, transport.applyIdempotencyKeys[1])
        assertNull(restarted.state.value.pendingProposalApplicationMutation)
    }

    @Test
    fun typedApplyNoEffectConflictClearsJournalOnlyAfterNotFoundLookupAndExactReplay() =
        runBlocking {
            val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
            val transport = FakeProposalApplicationsTransport().apply {
                applyFailure = ProposalApplicationApiException.Conflict(
                    RemoteProposalConflictCode.PREVIEW_EXPIRED,
                )
            }
            val manager = manager(store, transport)

            assertTrue(manager.prepareReview(PROPOSAL_ID))
            assertFalse(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))

            assertEquals(2, transport.applyCalls)
            assertEquals(1, transport.proposalLookupCalls)
            assertNull(store.state.value.pendingProposalApplicationMutation)
            assertTrue(store.state.value.proposalApplications.isEmpty())
        }

    @Test
    fun genericHttpRejectionsNeverClearAnUncertainApplyJournal() = runBlocking {
        listOf(
            ProposalApplicationApiException.Authorization(),
            ProposalApplicationApiException.NotFound(),
            ProposalApplicationApiException.Conflict(),
            ProposalApplicationApiException.Validation(422),
        ).forEach { failure ->
            val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
            val transport = FakeProposalApplicationsTransport().apply {
                applyFailure = failure
            }
            val manager = manager(store, transport)

            assertTrue(manager.prepareReview(PROPOSAL_ID))
            assertFalse(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))

            assertEquals(2, transport.applyCalls)
            assertEquals(1, transport.proposalLookupCalls)
            assertNotNull(store.state.value.pendingProposalApplicationMutation)
        }
    }

    @Test
    fun lostUndoResponseIsRecoveredByApplicationBeforeAnyReplay() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        val manager = manager(store, transport)
        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertTrue(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))
        transport.undoFailure = IOException("synthetic lost undo response")
        transport.applicationLookup = undoneReceipt()

        assertTrue(manager.undo(PROPOSAL_ID))

        assertEquals(1, transport.undoCalls)
        assertEquals(1, transport.applicationLookupCalls)
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            ProposalApplicationStatusSnapshot.UNDONE,
            store.state.value.proposalApplications.getValue(PROPOSAL_ID).status,
        )
    }

    @Test
    fun typedUndoNoEffectConflictClearsJournalAndRetainsAppliedReceipt() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        val manager = manager(store, transport)
        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertTrue(manager.applyReviewed(requireNotNull(manager.state.value.exactApproval)))
        transport.undoFailure = ProposalApplicationApiException.Conflict(
            RemoteProposalConflictCode.UNDO_EXPIRED,
        )

        assertFalse(manager.undo(PROPOSAL_ID))

        assertEquals(2, transport.undoCalls)
        assertEquals(1, transport.applicationLookupCalls)
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            ProposalApplicationStatusSnapshot.APPLIED,
            store.state.value.proposalApplications.getValue(PROPOSAL_ID).status,
        )
    }

    @Test
    fun unknownReservedSchemaFailsClosedWithoutTransportCall() = runBlocking {
        val proposal = typedProposal().copy(remotePayloadSchema = "dayweave.proposal-change-set/2")
        val transport = FakeProposalApplicationsTransport()
        val manager = manager(
            PlannerStore(DayWeaveUiState(suggestions = listOf(proposal))),
            transport,
        )

        assertFalse(manager.prepareReview(PROPOSAL_ID))

        assertEquals(0, transport.previewCalls)
        assertEquals(ProposalApplicationPhase.ERROR, manager.state.value.phase)
    }

    @Test
    fun approvalRejectsEveryCrossReviewBindingMismatchBeforeStagingOrNetwork() = runBlocking {
        val mutations: List<(ProposalApplicationApproval) -> ProposalApplicationApproval> = listOf(
            { approval -> approval.copy(proposalId = OTHER_PROPOSAL_ID) },
            { approval -> approval.copy(expectedProposalRevision = 2) },
            { approval -> approval.copy(previewId = OTHER_PREVIEW_ID) },
            { approval -> approval.copy(reviewHash = OTHER_REVIEW_HASH) },
        )
        mutations.forEach { mutate ->
            val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
            val transport = FakeProposalApplicationsTransport()
            val manager = manager(store, transport)
            assertTrue(manager.prepareReview(PROPOSAL_ID))

            val exactApproval = requireNotNull(manager.state.value.exactApproval)
            assertFalse(manager.applyReviewed(mutate(exactApproval)))

            assertEquals(0, transport.applyCalls)
            assertNull(store.state.value.pendingProposalApplicationMutation)
        }
    }

    @Test
    fun approvalFromSupersededReviewCannotApplyNewPreview() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        val manager = manager(store, transport)
        assertTrue(manager.prepareReview(PROPOSAL_ID))
        val superseded = requireNotNull(manager.state.value.exactApproval)
        transport.previewResponse = applicationPreview().copy(
            previewId = OTHER_PREVIEW_ID,
            reviewHash = OTHER_REVIEW_HASH,
        )
        assertTrue(manager.prepareReview(PROPOSAL_ID))

        assertFalse(manager.applyReviewed(superseded))

        assertEquals(0, transport.applyCalls)
        assertNull(store.state.value.pendingProposalApplicationMutation)
    }

    @Test
    fun expiredReviewCanBeRegeneratedAndConfigurationQuarantineClearsIt() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport().apply {
            previewResponse = applicationPreview().copy(expiresAt = "2026-08-30T09:59:59Z")
        }
        val manager = manager(store, transport)

        assertFalse(manager.prepareReview(PROPOSAL_ID))
        assertNull(manager.state.value.preview)
        transport.previewResponse = applicationPreview().copy(
            previewId = OTHER_PREVIEW_ID,
            reviewHash = OTHER_REVIEW_HASH,
        )
        assertTrue(manager.prepareReview(PROPOSAL_ID))
        assertNotNull(manager.state.value.preview)

        manager.quarantineBindingState()

        assertNull(manager.state.value.preview)
        assertNull(manager.state.value.exactApproval)
    }

    @Test
    fun privacyBoundaryDropsReviewButDoesNotCorruptJournaledApply() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        val manager = manager(store, transport)
        assertTrue(manager.prepareReview(PROPOSAL_ID))
        manager.discardReviewForPrivacyBoundary()
        assertNull(manager.state.value.preview)

        assertTrue(manager.prepareReview(PROPOSAL_ID))
        val approval = requireNotNull(manager.state.value.exactApproval)
        val applyStarted = CompletableDeferred<Unit>()
        val releaseApply = CompletableDeferred<Unit>()
        transport.applyStarted = applyStarted
        transport.applyRelease = releaseApply
        val result = async { manager.applyReviewed(approval) }
        applyStarted.await()
        val journal = requireNotNull(store.state.value.pendingProposalApplicationMutation)

        manager.discardReviewForPrivacyBoundary()

        assertNull(manager.state.value.preview)
        assertEquals(journal, store.state.value.pendingProposalApplicationMutation)
        releaseApply.complete(Unit)
        assertTrue(result.await())
        assertNull(store.state.value.pendingProposalApplicationMutation)
        assertEquals(
            ProposalApplicationStatusSnapshot.APPLIED,
            store.state.value.proposalApplications.getValue(PROPOSAL_ID).status,
        )
    }

    @Test
    fun privacyBoundaryPreventsInflightPreviewFromReappearing() = runBlocking {
        val store = PlannerStore(DayWeaveUiState(suggestions = listOf(typedProposal())))
        val transport = FakeProposalApplicationsTransport()
        val previewStarted = CompletableDeferred<Unit>()
        val releasePreview = CompletableDeferred<Unit>()
        transport.previewStarted = previewStarted
        transport.previewRelease = releasePreview
        val manager = manager(store, transport)
        val result = async { manager.prepareReview(PROPOSAL_ID) }
        previewStarted.await()

        manager.discardReviewForPrivacyBoundary()
        releasePreview.complete(Unit)

        assertFalse(result.await())
        assertNull(manager.state.value.preview)
        assertNull(manager.state.value.exactApproval)
    }

    private fun manager(
        store: PlannerStore,
        transport: FakeProposalApplicationsTransport,
    ) = ProposalApplicationManager(
        plannerStore = store,
        credentialStore = ProposalCredentialStore(),
        transport = transport,
        nowEpochMillis = { now },
    )

    private fun typedProposal() = PlanningSuggestion(
        id = PROPOSAL_ID,
        title = "Create a focus task",
        summary = "Create one reviewed task",
        source = "Codex",
        kind = SuggestionKind.NEW_TASK,
        expiresInDays = 2,
        remoteRevision = 1,
        remotePayloadSchema = "dayweave.proposal-change-set/1",
        remoteCreatedAt = "2026-08-30T09:00:00Z",
        remoteExpiresAt = "2026-09-01T10:00:00Z",
    )

    companion object {
        const val PROPOSAL_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val PREVIEW_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val OTHER_PREVIEW_ID = "b1111111-1111-4111-8111-111111111111"
        const val OTHER_PROPOSAL_ID = "a1111111-1111-4111-8111-111111111111"
        const val COMMAND_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val ITEM_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val APPLICATION_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        const val REVIEW_HASH =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        const val OTHER_REVIEW_HASH =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    }
}

private class ProposalCredentialStore : ApiCredentialStore {
    private val configuration = AuthenticatedApiConfiguration.createBound(
        "https://api.example.test/",
        "synthetic-token",
        "connection-1",
    )

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = configuration.baseUrl.toString(),
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = "connection-1",
    )

    override fun authenticatedConfiguration() = configuration
    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeProposalApplicationsTransport : ProposalApplicationsTransport {
    var previewCalls = 0
    var applyCalls = 0
    var proposalLookupCalls = 0
    var applicationLookupCalls = 0
    var undoCalls = 0
    var applyFailure: Throwable? = null
    var proposalLookupFailure: Throwable? = null
    var proposalLookup: RemoteProposalApplicationReceipt? = null
    var applicationLookupFailure: Throwable? = null
    var applicationLookup: RemoteProposalApplicationReceipt? = null
    var undoFailure: Throwable? = null
    var onApply: (() -> Unit)? = null
    var previewResponse: RemoteProposalApplicationPreview = applicationPreview()
    var previewStarted: CompletableDeferred<Unit>? = null
    var previewRelease: CompletableDeferred<Unit>? = null
    var applyStarted: CompletableDeferred<Unit>? = null
    var applyRelease: CompletableDeferred<Unit>? = null
    val applyRequests = mutableListOf<ProposalApplicationHttpRequest>()
    val applyIdempotencyKeys = mutableListOf<String>()

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        request: ProposalPreviewRequest,
    ): RemoteProposalApplicationPreview {
        previewCalls += 1
        previewStarted?.complete(Unit)
        previewRelease?.await()
        return previewResponse
    }

    override suspend fun apply(
        configuration: AuthenticatedApiConfiguration,
        previewId: String,
        expectedReviewHash: String,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalApplyResponse {
        applyCalls += 1
        applyRequests += request
        applyIdempotencyKeys += idempotencyKey
        onApply?.invoke()
        applyStarted?.complete(Unit)
        applyRelease?.await()
        applyFailure?.let { throw it }
        return RemoteProposalApplyResponse(appliedReceipt(), replayed = false)
    }

    override suspend fun getById(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
    ): RemoteProposalApplicationReceipt {
        applicationLookupCalls += 1
        applicationLookupFailure?.let { throw it }
        return applicationLookup ?: appliedReceipt()
    }

    override suspend fun getByProposal(
        configuration: AuthenticatedApiConfiguration,
        proposalId: String,
    ): RemoteProposalApplicationReceipt {
        proposalLookupCalls += 1
        proposalLookupFailure?.let { throw it }
        return proposalLookup ?: throw ProposalApplicationApiException.NotFound()
    }

    override suspend fun undo(
        configuration: AuthenticatedApiConfiguration,
        applicationId: String,
        expectedApplicationRevision: Long,
        idempotencyKey: String,
        request: ProposalApplicationHttpRequest,
    ): RemoteProposalUndoResponse {
        undoCalls += 1
        undoFailure?.let { throw it }
        return RemoteProposalUndoResponse(
            application = undoneReceipt(),
            replayed = false,
        )
    }
}

private fun applicationPreview() = RemoteProposalApplicationPreview(
    previewId = ProposalApplicationManagerTest.PREVIEW_ID,
    proposals = listOf(
        ProposalPreviewMember(
            proposalId = ProposalApplicationManagerTest.PROPOSAL_ID,
            expectedRevision = 1,
        ),
    ),
    changeSetSchema = "dayweave.proposal-change-set/1",
    commandIds = listOf(ProposalApplicationManagerTest.COMMAND_ID),
    reviewHash = ProposalApplicationManagerTest.REVIEW_HASH,
    expiresAt = "2026-08-31T10:00:00Z",
    canApply = true,
    maximumRisk = RemoteProposalRiskLevel.LOW,
    requiresExplicitApproval = false,
    diffs = listOf(
        RemoteProposalItemDiff(
            commandId = ProposalApplicationManagerTest.COMMAND_ID,
            operation = RemoteProposalOperation.CREATE_ITEM,
            itemId = ProposalApplicationManagerTest.ITEM_ID,
            changedFields = listOf(RemoteProposalItemField.TITLE),
            before = null,
            after = proposalItem(),
        ),
    ),
    implicitDiffs = emptyList(),
    risks = emptyList(),
    conflicts = emptyList(),
)

private fun proposalItem() = RemoteProposalCanonicalItem(
    id = ProposalApplicationManagerTest.ITEM_ID,
    isSensitive = false,
    kind = RemoteProposalItemKind.TASK,
    status = RemoteProposalItemStatus.PLANNED,
    title = "Focus task",
    notes = null,
    timezoneName = "UTC",
    durationSeconds = 1_800,
    deadlineAt = null,
    earliestStartAt = null,
    recurrence = null,
    flexibleConstraints = buildJsonObject { },
    splitPolicy = buildJsonObject { put("type", "indivisible") },
    importance = 50,
    urgency = 50,
    parentId = null,
    siblingOrder = 0,
    isExecutable = true,
    revision = 1,
    createdAt = "2026-08-30T10:00:00Z",
    updatedAt = "2026-08-30T10:00:00Z",
    completedAt = null,
    deletedAt = null,
)

private fun appliedReceipt() = RemoteProposalApplicationReceipt(
    applicationId = ProposalApplicationManagerTest.APPLICATION_ID,
    proposals = listOf(
        RemoteProposalAppliedMember(
            proposalId = ProposalApplicationManagerTest.PROPOSAL_ID,
            appliedRevision = 2,
        ),
    ),
    applicationRevision = 1,
    status = RemoteProposalApplicationStatus.APPLIED,
    commandIds = listOf(ProposalApplicationManagerTest.COMMAND_ID),
    affectedItemIds = listOf(ProposalApplicationManagerTest.ITEM_ID),
    appliedAt = "2026-08-30T10:01:00Z",
    undoExpiresAt = "2026-08-31T10:01:00Z",
    undoneAt = null,
)

private fun undoneReceipt() = appliedReceipt().copy(
    status = RemoteProposalApplicationStatus.UNDONE,
    applicationRevision = 2,
    undoneAt = "2026-08-30T10:10:00Z",
)
