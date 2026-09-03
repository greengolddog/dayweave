package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.data.PlannerStateRepository
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.GoogleSchedulePublicationJournal
import com.greengolddog.dayweave.model.GoogleSchedulePublicationStage
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleSchedulePublicationTransport
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationAccepted
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationApproval
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationChange
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationPreview
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationStatus
import com.greengolddog.dayweave.network.ScheduleGooglePublicationOperation
import com.greengolddog.dayweave.network.ScheduleGooglePublicationState
import com.greengolddog.dayweave.state.PlannerStore
import com.greengolddog.dayweave.ui.authoring.googleScheduleRecoveryRequiresConfirmation
import java.time.Instant
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import com.greengolddog.dayweave.state.PlannerLoadState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleSchedulePublicationCoordinatorTest {
    @Test
    fun onlyApprovedRecoveryRequiresExternalEffectConfirmation() {
        GoogleSchedulePublicationStage.entries.forEach { stage ->
            assertEquals(
                stage == GoogleSchedulePublicationStage.APPROVED,
                googleScheduleRecoveryRequiresConfirmation(stage),
            )
        }
        assertFalse(googleScheduleRecoveryRequiresConfirmation(null))
    }

    @Test
    fun approvedRecoveryRequiresExplicitReplayEvenAfterLocalExpiry() = runBlocking {
        val store = PlannerStore(boundState(approvedJournal()))
        val transport = FakeSchedulePublicationTransport()
        val coordinator = coordinator(
            store,
            transport,
            now = { Instant.parse("2026-09-03T12:21:00Z") },
        )

        val automaticOutcome = coordinator.recoverPending()

        assertEquals(GoogleSchedulePublicationOutcome.PENDING, automaticOutcome)
        assertEquals(0, transport.enqueueCalls)
        assertEquals(0, transport.statusCalls)
        assertEquals(0, transport.approvalCalls)
        assertEquals(
            GoogleSchedulePublicationPhase.APPROVED_REPLAY_REQUIRED,
            coordinator.state.value.phase,
        )
        assertEquals(
            GoogleSchedulePublicationStage.APPROVED,
            store.durableState.value?.pendingGoogleSchedulePublication?.stage,
        )

        val outcome = coordinator.replayApprovedEnqueue()

        assertEquals(GoogleSchedulePublicationOutcome.STATUS_UPDATED, outcome)
        assertEquals(1, transport.enqueueCalls)
        assertEquals(1, transport.statusCalls)
        assertEquals(0, transport.approvalCalls)
        val recovered = requireNotNull(store.durableState.value?.pendingGoogleSchedulePublication)
        assertEquals(GoogleSchedulePublicationStage.ACCEPTED, recovered.stage)
        assertNull(recovered.approvalCapability)
        assertNull(recovered.approvalExpiresAt)
        assertEquals(ScheduleGooglePublicationState.PUBLISHED, recovered.status?.state)
        assertEquals(GoogleSchedulePublicationPhase.PUBLISHED, coordinator.state.value.phase)
    }

    @Test
    fun intentAndPreviewRecoveryRemainFencedAtLocalExpiry() = runBlocking {
        val cases = listOf(
            intentJournal() to Instant.parse("2026-09-03T12:31:00Z"),
            intentJournal().recordingPreview(validPreview()) to
                Instant.parse("2026-09-03T12:21:00Z"),
        )

        cases.forEachIndexed { index, (journal, clock) ->
            val transport = FakeSchedulePublicationTransport()
            val coordinator = coordinator(
                PlannerStore(boundState(journal)),
                transport,
                now = { clock },
            )

            assertEquals(
                "expired recovery case $index",
                GoogleSchedulePublicationOutcome.EXPIRED,
                coordinator.recoverPending(),
            )
            assertEquals(0, transport.previewCalls)
            assertEquals(0, transport.approvalCalls)
            assertEquals(0, transport.enqueueCalls)
            assertEquals(0, transport.statusCalls)
        }
    }

    @Test
    fun locallyElapsedApprovalResponseWithinClockSkewIsPersistedAndEnqueued() = runBlocking {
        val previewed = intentJournal().recordingPreview(validPreview())
        val store = PlannerStore(publishedState(previewed))
        val transport = FakeSchedulePublicationTransport().apply {
            onApproval = {
                RemoteScheduleGooglePublicationApproval(
                    PREVIEW_ID,
                    CAPABILITY,
                    "2026-09-03T12:04:00Z",
                )
            }
        }
        val coordinator = coordinator(store, transport)
        assertEquals(
            GoogleSchedulePublicationOutcome.PREVIEW_READY,
            coordinator.recoverPending(),
        )

        assertEquals(
            GoogleSchedulePublicationOutcome.STATUS_UPDATED,
            coordinator.approveAndEnqueue(requireNotNull(coordinator.approvalConfirmation())),
        )
        assertEquals(1, transport.approvalCalls)
        assertEquals(1, transport.enqueueCalls)
        assertEquals(1, transport.statusCalls)
        val persisted = requireNotNull(
            store.durableState.value?.pendingGoogleSchedulePublication,
        )
        assertEquals(GoogleSchedulePublicationStage.ACCEPTED, persisted.stage)
        assertNull(persisted.approvalCapability)
    }

    @Test
    fun privacyBoundaryHidesPendingDestinationMetadata() {
        var operationAllowed = false
        val coordinator = coordinator(
            PlannerStore(boundState(approvedJournal())),
            FakeSchedulePublicationTransport(),
            operationAllowed = { operationAllowed },
        )

        assertNull(coordinator.pendingDestinationOption())
        operationAllowed = true
        assertEquals("Private Gmail · Planning", coordinator.pendingDestinationOption()?.displayName)
    }

    @Test
    fun pendingGoogleImportRecoveryBlocksScheduleTargetsAndFreshPreview() = runBlocking {
        val transport = FakeSchedulePublicationTransport()
        val coordinator = coordinator(
            PlannerStore(publishedState()),
            transport,
            imports = publicationImportState().copy(pendingRecoveryCount = 1),
        )

        assertTrue(coordinator.targets().isEmpty())
        assertEquals(
            GoogleSchedulePublicationOutcome.FAILED,
            coordinator.preparePreview(PUBLICATION_TARGET),
        )
        assertEquals(0, transport.previewCalls)
    }

    @Test
    fun durableIntentFencePreventsPreviewAfterPrivacyBoundary() = runBlocking {
        val initial = publishedState()
        val repository = GatedSchedulePublicationRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        var operationAllowed = true
        val transport = FakeSchedulePublicationTransport()
        lateinit var coordinator: GoogleSchedulePublicationCoordinator

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            coordinator = coordinator(
                store,
                transport,
                operationAllowed = { operationAllowed },
            )
            val operation = async { coordinator.preparePreview(PUBLICATION_TARGET) }
            val saved = withTimeout(3_000) { repository.saveStarted.receive() }
            assertEquals(
                GoogleSchedulePublicationStage.INTENT,
                saved.pendingGoogleSchedulePublication?.stage,
            )

            operationAllowed = false
            coordinator.quarantineBindingState()
            repository.allowSave.send(Unit)

            assertEquals(
                GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
                withTimeout(3_000) { operation.await() },
            )
            assertEquals(0, transport.previewCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun durableApprovedFencePreventsEnqueueAfterBindingChange() = runBlocking {
        val initial = publishedState(intentJournal().recordingPreview(validPreview()))
        val repository = GatedSchedulePublicationRepository(initial)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val credentials = FakeSchedulePublicationCredentials()
        val transport = FakeSchedulePublicationTransport().apply {
            onApproval = {
                RemoteScheduleGooglePublicationApproval(
                    PREVIEW_ID,
                    CAPABILITY,
                    "2026-09-03T12:15:00Z",
                )
            }
        }

        try {
            val store = PlannerStore(initial, repository, scope)
            withTimeout(3_000) { store.loadState.first { it == PlannerLoadState.READY } }
            val coordinator = coordinator(store, transport, credentials = credentials)
            assertEquals(
                GoogleSchedulePublicationOutcome.PREVIEW_READY,
                coordinator.recoverPending(),
            )
            val confirmation = requireNotNull(coordinator.approvalConfirmation())
            val operation = async { coordinator.approveAndEnqueue(confirmation) }

            assertEquals(
                GoogleSchedulePublicationStage.APPROVAL_ATTEMPTED,
                withTimeout(3_000) { repository.saveStarted.receive() }
                    .pendingGoogleSchedulePublication?.stage,
            )
            repository.allowSave.send(Unit)
            assertEquals(
                GoogleSchedulePublicationStage.APPROVED,
                withTimeout(3_000) { repository.saveStarted.receive() }
                    .pendingGoogleSchedulePublication?.stage,
            )
            credentials.configurationId = CONFIGURATION_B
            repository.allowSave.send(Unit)

            assertEquals(
                GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
                withTimeout(3_000) { operation.await() },
            )
            assertEquals(1, transport.approvalCalls)
            assertEquals(0, transport.enqueueCalls)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun cancelledHolderAndNewLifecyclePreventWaitingRecoveryFromStartingRequest() = runBlocking {
        val store = PlannerStore(boundState(approvedJournal()))
        val enqueueEntered = CompletableDeferred<Unit>()
        val neverReturns = CompletableDeferred<RemoteScheduleGooglePublicationAccepted>()
        val transport = FakeSchedulePublicationTransport().apply {
            onEnqueue = {
                enqueueEntered.complete(Unit)
                neverReturns.await()
            }
        }
        var operationAllowed = true
        val coordinator = coordinator(
            store,
            transport,
            operationAllowed = { operationAllowed },
        )
        val holding = async { coordinator.replayApprovedEnqueue() }
        withTimeout(3_000) { enqueueEntered.await() }
        val waiting = async(start = CoroutineStart.UNDISPATCHED) {
            coordinator.replayApprovedEnqueue()
        }

        operationAllowed = false
        coordinator.quarantineBindingState()
        holding.cancelAndJoin()

        assertEquals(
            GoogleSchedulePublicationOutcome.RECOVERY_REQUIRED,
            withTimeout(3_000) { waiting.await() },
        )
        assertEquals(1, transport.enqueueCalls)
    }

    @Test
    fun queuedExpiredDiscardCannotCrossLifecycleQuarantine() = runBlocking {
        var clock = NOW
        var operationAllowed = true
        val previewEntered = CompletableDeferred<Unit>()
        val neverReturns = CompletableDeferred<RemoteScheduleGooglePublicationPreview>()
        val store = PlannerStore(publishedState())
        val transport = FakeSchedulePublicationTransport().apply {
            onPreview = {
                previewEntered.complete(Unit)
                neverReturns.await()
            }
        }
        val coordinator = coordinator(
            store,
            transport,
            now = { clock },
            operationAllowed = { operationAllowed },
        )
        val holding = async { coordinator.preparePreview(PUBLICATION_TARGET) }

        try {
            withTimeout(3_000) { previewEntered.await() }
            val journalBeforeQuarantine = requireNotNull(
                store.durableState.value?.pendingGoogleSchedulePublication,
            )
            assertEquals(GoogleSchedulePublicationStage.INTENT, journalBeforeQuarantine.stage)
            clock = Instant.parse("2026-09-03T13:00:00Z")
            val waiting = async(start = CoroutineStart.UNDISPATCHED) {
                coordinator.discardExpiredRecovery()
            }

            operationAllowed = false
            coordinator.quarantineBindingState()
            operationAllowed = true
            holding.cancelAndJoin()

            assertFalse(withTimeout(3_000) { waiting.await() })
            assertEquals(
                journalBeforeQuarantine,
                store.durableState.value?.pendingGoogleSchedulePublication,
            )
            assertEquals(
                GoogleSchedulePublicationPhase.PRIVACY_PROTECTED,
                coordinator.state.value.phase,
            )
        } finally {
            holding.cancelAndJoin()
        }
    }

    @Test
    fun queuedSettledDismissCannotCrossLifecycleQuarantine() = runBlocking {
        var operationAllowed = true
        val statusEntered = CompletableDeferred<Unit>()
        val neverReturns = CompletableDeferred<RemoteScheduleGooglePublicationStatus>()
        val settled = settledJournal()
        val store = PlannerStore(boundState(settled))
        val transport = FakeSchedulePublicationTransport().apply {
            onStatus = { _, _ ->
                statusEntered.complete(Unit)
                neverReturns.await()
            }
        }
        val coordinator = coordinator(
            store,
            transport,
            operationAllowed = { operationAllowed },
        )
        val holding = async { coordinator.recoverPending() }

        try {
            withTimeout(3_000) { statusEntered.await() }
            val waiting = async(start = CoroutineStart.UNDISPATCHED) {
                coordinator.dismissSettled()
            }

            operationAllowed = false
            coordinator.quarantineBindingState()
            operationAllowed = true
            holding.cancelAndJoin()

            assertFalse(withTimeout(3_000) { waiting.await() })
            assertEquals(settled, store.durableState.value?.pendingGoogleSchedulePublication)
            assertEquals(
                GoogleSchedulePublicationPhase.PRIVACY_PROTECTED,
                coordinator.state.value.phase,
            )
        } finally {
            holding.cancelAndJoin()
        }
    }

    @Test
    fun unknownOneShotApprovalIsNeverRetriedByRecovery() = runBlocking {
        val store = PlannerStore(boundState(attemptedJournal()))
        val transport = FakeSchedulePublicationTransport()
        val coordinator = coordinator(store, transport)

        assertEquals(GoogleSchedulePublicationOutcome.PENDING, coordinator.recoverPending())
        assertEquals(0, transport.approvalCalls)
        assertEquals(0, transport.enqueueCalls)
        assertEquals(GoogleSchedulePublicationPhase.RESPONSE_UNKNOWN, coordinator.state.value.phase)
        assertTrue(store.state.value.pendingGoogleSchedulePublication != null)
    }

    private fun coordinator(
        store: PlannerStore,
        transport: FakeSchedulePublicationTransport,
        credentials: FakeSchedulePublicationCredentials = FakeSchedulePublicationCredentials(),
        now: () -> Instant = { NOW },
        operationAllowed: () -> Boolean = { true },
        imports: GoogleCalendarImportState = publicationImportState(),
    ) = GoogleSchedulePublicationCoordinator(
        plannerStore = store,
        credentialStore = credentials,
        transport = transport,
        googleAccountState = { publicationAccountState() },
        googleImportState = { imports },
        now = now,
        newUuid = { UUID.fromString(RECOVERY_ID) },
        operationAllowed = operationAllowed,
    )

    private fun boundState(journal: GoogleSchedulePublicationJournal) = DayWeaveUiState(
        canonicalSyncOrigin = API_BASE_URL,
        canonicalConfigurationId = CONFIGURATION_ID,
        pendingGoogleSchedulePublication = journal,
    )

    private fun publicationAccountState() = GoogleAccountState(
        phase = GoogleAccountPhase.CONNECTED,
        accounts = listOf(
            GoogleAccountSummary(
                id = ACCOUNT_ID,
                label = "Private Gmail",
                status = "active",
                syncEnabled = true,
                isDefault = true,
                hasCalendar = true,
                hasCalendarWriteScope = true,
                hasTasks = false,
                hasTasksWriteScope = false,
                revision = 3,
            ),
        ),
        message = "Ready",
        configurationId = CONFIGURATION_ID,
    )

    private fun publicationImportState() = GoogleCalendarImportState(
        phase = GoogleCalendarImportPhase.READY,
        message = "Ready",
        accounts = mapOf(
            ACCOUNT_ID to GoogleImportAccountState(
                collections = listOf(
                    GoogleImportCollectionState(
                        id = COLLECTION_ID,
                        accountId = ACCOUNT_ID,
                        displayName = "Planning",
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        providerDeleted = false,
                        selected = true,
                        visible = true,
                        syncRole = RemoteGoogleSyncRole.WRITABLE,
                        calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
                        revision = 7,
                        lastImportAt = "2026-09-03T12:00:00Z",
                        providerAccessRole = "owner",
                    ),
                ),
            ),
        ),
        configurationId = CONFIGURATION_ID,
    )

    private fun publishedState(
        journal: GoogleSchedulePublicationJournal? = null,
    ): DayWeaveUiState {
        val digest = "sha256:${"b".repeat(64)}"
        val revision = PublishedScheduleRevisionSnapshot(
            id = SCHEDULE_REVISION_ID,
            revision = "11:$SCHEDULE_REVISION_ID",
            revisionNumber = 11uL,
            inputDigest = digest,
            horizonStart = "2026-09-03T00:00:00Z",
            horizonEnd = "2026-09-04T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-09-03T12:00:00Z",
        )
        return DayWeaveUiState(
            canonicalSyncOrigin = API_BASE_URL,
            canonicalConfigurationId = CONFIGURATION_ID,
            canonicalDeltaCursor = "cursor-11",
            publishedScheduleRevision = revision,
            publishedScheduleProof = PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = API_BASE_URL,
                configurationId = CONFIGURATION_ID,
                revision = revision,
                asOf = "2026-09-03T12:00:00Z",
                blocks = emptyList(),
            ),
            scheduleInputDigest = digest,
            scheduleGeneratedAt = "2026-09-03T12:00:00Z",
            schedulePlanningZoneId = "UTC",
            pendingGoogleSchedulePublication = journal,
        )
    }

    private fun intentJournal() = GoogleSchedulePublicationJournal(
        recoveryId = RECOVERY_ID,
        operationGeneration = 1,
        configurationId = CONFIGURATION_ID,
        apiBaseUrl = API_BASE_URL,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        expectedScheduleRevisionId = SCHEDULE_REVISION_ID,
        intentExpiresAt = "2026-09-03T12:30:00Z",
        createdAt = "2026-09-03T12:00:00Z",
    )

    private fun attemptedJournal() = intentJournal().recordingPreview(validPreview())
        .recordingApprovalAttempt()

    private fun approvedJournal() = attemptedJournal().recordingApproval(
        RemoteScheduleGooglePublicationApproval(
            PREVIEW_ID,
            CAPABILITY,
            "2026-09-03T12:15:00Z",
        ),
    )

    private fun settledJournal() = approvedJournal()
        .recordingAcceptance(
            RemoteScheduleGooglePublicationAccepted(PUBLICATION_ID, replayed = false),
        )
        .recordingStatus(
            RemoteScheduleGooglePublicationStatus(
                publicationId = PUBLICATION_ID,
                accountId = ACCOUNT_ID,
                collectionId = COLLECTION_ID,
                scheduleRevisionId = SCHEDULE_REVISION_ID,
                state = ScheduleGooglePublicationState.PUBLISHED,
                totalCount = 1,
                pendingCount = 0,
                deliveringCount = 0,
                publishedCount = 1,
                conflictedCount = 0,
                failedCount = 0,
                supersededCount = 0,
                createdAt = "2026-09-03T12:06:00Z",
                completedAt = "2026-09-03T12:07:00Z",
                lastErrorCode = null,
            ),
        )

    private fun validPreview() = RemoteScheduleGooglePublicationPreview(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = 7,
        collectionDisplayName = "Planning",
        scheduleRevisionId = SCHEDULE_REVISION_ID,
        scheduleRevisionNumber = 11,
        previewHash = "a".repeat(64),
        createCount = 1,
        updateCount = 0,
        deleteCount = 0,
        noopCount = 0,
        changes = listOf(
            RemoteScheduleGooglePublicationChange(
                ordinal = 0,
                slotId = SLOT_ID,
                sourceBlockId = SOURCE_BLOCK_ID,
                operation = ScheduleGooglePublicationOperation.CREATE,
                providerResourceId = null,
                providerEtag = null,
                summary = "Focus block",
                startsAt = "2026-09-03T13:00:00Z",
                endsAt = "2026-09-03T14:00:00Z",
            ),
        ),
        expiresAt = "2026-09-03T12:20:00Z",
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-03T12:05:00Z")
        const val API_BASE_URL = "https://api.example.test/"
        const val CONFIGURATION_ID = "configuration-a"
        const val CONFIGURATION_B = "configuration-b"
        const val RECOVERY_ID = "11111111-1111-4111-8111-111111111111"
        const val ACCOUNT_ID = "22222222-2222-4222-8222-222222222222"
        const val COLLECTION_ID = "33333333-3333-4333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PREVIEW_ID = "55555555-5555-4555-8555-555555555555"
        const val SLOT_ID = "66666666-6666-4666-8666-666666666666"
        const val SOURCE_BLOCK_ID = "77777777-7777-4777-8777-777777777777"
        const val PUBLICATION_ID = "88888888-8888-4888-8888-888888888888"
        val CAPABILITY = "dw_gsa1_" + Base64.getUrlEncoder().withoutPadding()
            .encodeToString(ByteArray(32) { (it + 1).toByte() })
        val PUBLICATION_TARGET = com.greengolddog.dayweave.model.GoogleSchedulePublicationTarget(
            ACCOUNT_ID,
            COLLECTION_ID,
            7,
        )
    }
}

private class GatedSchedulePublicationRepository(
    private val initial: DayWeaveUiState,
) : PlannerStateRepository {
    val saveStarted = Channel<DayWeaveUiState>(Channel.UNLIMITED)
    val allowSave = Channel<Unit>(Channel.UNLIMITED)

    override suspend fun load(): DayWeaveUiState = initial

    override suspend fun save(state: DayWeaveUiState) {
        saveStarted.send(state)
        allowSave.receive()
    }
}

private class FakeSchedulePublicationCredentials : ApiCredentialStore {
    var configurationId = "configuration-a"

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = true,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration() = AuthenticatedApiConfiguration.createBound(
        baseUrl = "https://api.example.test/",
        bearerToken = "synthetic-secret",
        configurationId = configurationId,
    )

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeSchedulePublicationTransport : GoogleSchedulePublicationTransport {
    var previewCalls = 0
    var approvalCalls = 0
    var enqueueCalls = 0
    var statusCalls = 0
    var onPreview: suspend () -> RemoteScheduleGooglePublicationPreview = {
        error("Unexpected preview")
    }
    var onApproval: suspend () -> RemoteScheduleGooglePublicationApproval = {
        error("Approval must not be retried")
    }
    var onEnqueue: suspend () -> RemoteScheduleGooglePublicationAccepted = {
        RemoteScheduleGooglePublicationAccepted(
            "88888888-8888-4888-8888-888888888888",
            replayed = true,
        )
    }
    var onStatus: suspend (String, String) -> RemoteScheduleGooglePublicationStatus =
        { accountId, publicationId -> publishedStatus(accountId, publicationId) }

    override suspend fun previewSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
    ): RemoteScheduleGooglePublicationPreview {
        previewCalls += 1
        return onPreview()
    }

    override suspend fun approveSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteScheduleGooglePublicationApproval {
        approvalCalls += 1
        return onApproval()
    }

    override suspend fun enqueueSchedulePublication(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        collectionId: String,
        expectedScheduleRevisionId: String,
        approvalCapability: String,
    ): RemoteScheduleGooglePublicationAccepted {
        enqueueCalls += 1
        require(accountId == "22222222-2222-4222-8222-222222222222")
        require(previewId == "55555555-5555-4555-8555-555555555555")
        require(collectionId == "33333333-3333-4333-8333-333333333333")
        require(expectedScheduleRevisionId == "44444444-4444-4444-8444-444444444444")
        require(approvalCapability.startsWith("dw_gsa1_"))
        return onEnqueue()
    }

    override suspend fun schedulePublicationStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        publicationId: String,
    ): RemoteScheduleGooglePublicationStatus {
        statusCalls += 1
        return onStatus(accountId, publicationId)
    }

    private fun publishedStatus(
        accountId: String,
        publicationId: String,
    ): RemoteScheduleGooglePublicationStatus = RemoteScheduleGooglePublicationStatus(
            publicationId = publicationId,
            accountId = accountId,
            collectionId = "33333333-3333-4333-8333-333333333333",
            scheduleRevisionId = "44444444-4444-4444-8444-444444444444",
            state = ScheduleGooglePublicationState.PUBLISHED,
            totalCount = 1,
            pendingCount = 0,
            deliveringCount = 0,
            publishedCount = 1,
            conflictedCount = 0,
            failedCount = 0,
            supersededCount = 0,
            createdAt = "2026-09-03T12:06:00Z",
            completedAt = "2026-09-03T12:07:00Z",
            lastErrorCode = null,
        )
}
