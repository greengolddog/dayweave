package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.ConfigureGoogleCollectionRequest
import com.greengolddog.dayweave.network.GoogleCalendarInboundApiException
import com.greengolddog.dayweave.network.GoogleInboundCollectionRole
import com.greengolddog.dayweave.network.GoogleCalendarInboundTransport
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCalendarProjectionState
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleCollections
import com.greengolddog.dayweave.network.RemoteGoogleEventDisposition
import com.greengolddog.dayweave.network.RemoteGoogleSyncCollection
import com.greengolddog.dayweave.network.RemoteGoogleSyncRefreshAccepted
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunState
import com.greengolddog.dayweave.network.RemoteGoogleSyncRunStatus
import com.greengolddog.dayweave.network.RemoteGoogleSyncStatus
import java.io.IOException
import java.time.Instant
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleCalendarImportCoordinatorTest {
    @Test
    fun schedulePublicationAuthorityBlocksImportStartAndPostSaveRace() = runBlocking {
        val blockedTransport = FakeGoogleInboundTransport()
        val blockedJournals = InMemoryGoogleImportJournalStore()
        val blocked = coordinator(
            credentials = FakeGoogleImportCredentials(),
            transport = blockedTransport,
            journals = blockedJournals,
            pipeline = FakeGoogleImportPipeline(),
            importAllowed = { false },
        )

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            blocked.refresh(ACCOUNT_A),
        )
        assertTrue(blockedTransport.refreshRequestIds.isEmpty())
        assertEquals(0, blockedTransport.statusCalls)
        assertTrue(blockedJournals.journals.isEmpty())

        var importAllowed = true
        val racingTransport = FakeGoogleInboundTransport()
        val racingJournals = InMemoryGoogleImportJournalStore().apply {
            afterSave = { journal ->
                if (!journal.isAccepted) importAllowed = false
            }
        }
        val racing = coordinator(
            credentials = FakeGoogleImportCredentials(),
            transport = racingTransport,
            journals = racingJournals,
            pipeline = FakeGoogleImportPipeline(),
            importAllowed = { importAllowed },
        )

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            racing.refresh(ACCOUNT_A),
        )
        assertTrue(racingTransport.refreshRequestIds.isEmpty())
        assertEquals(0, racingTransport.statusCalls)
        assertEquals(1, racingJournals.journals.size)
        assertFalse(racingJournals.journals.single().isAccepted)
    }

    @Test
    fun lostRefreshResponseSurvivesRestartAndReplaysExactRequest() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val firstTransport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, _, _ -> throw IOException("synthetic lost response") }
        }
        val firstPipeline = FakeGoogleImportPipeline()
        val first = coordinator(credentials, firstTransport, journals, firstPipeline)

        assertEquals(
            GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            first.refresh(ACCOUNT_A),
        )
        val prepared = journals.journals.single()
        assertFalse(prepared.isAccepted)
        assertEquals(REQUEST_ID, prepared.requestId)
        assertEquals(listOf(UUID.fromString(REQUEST_ID)), firstTransport.refreshRequestIds)
        assertTrue(firstPipeline.inputs.isEmpty())

        val secondTransport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, accountId, requestId -> accepted(accountId, requestId, 7) }
            onStatus = { _, accountId -> status(run(accountId, RemoteGoogleSyncRunState.IDLE, 7, 7)) }
        }
        val secondPipeline = FakeGoogleImportPipeline()
        val relaunched = coordinator(credentials, secondTransport, journals, secondPipeline)

        assertEquals(
            GoogleCalendarImportOutcome.COMPLETED,
            relaunched.recoverPending(ACCOUNT_A),
        )
        assertEquals(listOf(UUID.fromString(REQUEST_ID)), secondTransport.refreshRequestIds)
        assertEquals(7L, secondPipeline.inputs.single().acceptedRefreshGeneration)
        assertTrue(journals.journals.isEmpty())
        assertFalse(relaunched.hasCredentialRecoveryBlocker())
    }

    @Test
    fun freshDefinitivePreAcceptanceRejectionRetiresIdentityAndRetryUsesANewUuid() = runBlocking {
        val definitiveRejections = listOf<Exception>(
            GoogleCalendarInboundApiException.Validation(400),
            GoogleCalendarInboundApiException.Validation(422),
            GoogleCalendarInboundApiException.NotFound(),
            GoogleCalendarInboundApiException.Conflict(),
            GoogleCalendarInboundApiException.Http(403),
        )

        for (rejection in definitiveRejections) {
            val credentials = FakeGoogleImportCredentials()
            val journals = InMemoryGoogleImportJournalStore()
            var refreshCall = 0
            val transport = FakeGoogleInboundTransport().apply {
                onRefresh = { _, accountId, requestId ->
                    if (refreshCall++ == 0) throw rejection
                    accepted(accountId, requestId, 2)
                }
                onStatus = { _, accountId ->
                    status(run(accountId, RemoteGoogleSyncRunState.IDLE, 2, 2))
                }
            }
            val requestIds = ArrayDeque(
                listOf(UUID.fromString(REQUEST_ID), UUID.fromString(REQUEST_ID_B)),
            )
            val pipeline = FakeGoogleImportPipeline()
            val coordinator = coordinator(
                credentials = credentials,
                transport = transport,
                journals = journals,
                pipeline = pipeline,
                newRequestId = { requestIds.removeFirst() },
            )

            assertEquals(GoogleCalendarImportOutcome.FAILED, coordinator.refresh(ACCOUNT_A))
            assertEquals(GoogleCalendarImportPhase.ERROR, coordinator.state.value.phase)
            assertEquals(0, coordinator.state.value.pendingRecoveryCount)
            assertTrue(journals.journals.isEmpty())
            assertEquals(1, journals.rejectedRetirementCount)
            assertFalse(coordinator.hasCredentialRecoveryBlocker())

            assertEquals(GoogleCalendarImportOutcome.COMPLETED, coordinator.refresh(ACCOUNT_A))
            assertEquals(
                listOf(UUID.fromString(REQUEST_ID), UUID.fromString(REQUEST_ID_B)),
                transport.refreshRequestIds,
            )
            assertEquals(1, pipeline.inputs.size)
            assertTrue(journals.journals.isEmpty())
        }
    }

    @Test
    fun preexistingPreparedReplayRetainsIdentityAfterDefinitiveRejection() = runBlocking {
        val replayRejections = listOf<Exception>(
            GoogleCalendarInboundApiException.NotFound(),
            GoogleCalendarInboundApiException.Conflict(),
            GoogleCalendarInboundApiException.Http(403),
        )

        for (rejection in replayRejections) {
            val prepared = preparedJournal()
            val journals = InMemoryGoogleImportJournalStore().apply {
                this.journals += prepared
            }
            val transport = FakeGoogleInboundTransport().apply {
                onRefresh = { _, _, _ -> throw rejection }
            }
            val coordinator = coordinator(
                credentials = FakeGoogleImportCredentials(),
                transport = transport,
                journals = journals,
                pipeline = FakeGoogleImportPipeline(),
            )

            assertEquals(
                GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
                coordinator.recoverPending(ACCOUNT_A),
            )
            assertEquals(listOf(UUID.fromString(REQUEST_ID)), transport.refreshRequestIds)
            assertEquals(listOf(prepared), journals.journals)
            assertEquals(0, journals.rejectedRetirementAttempts)
            assertEquals(GoogleCalendarImportPhase.RECOVERY_REQUIRED, coordinator.state.value.phase)
            assertTrue(coordinator.hasCredentialRecoveryBlocker())
        }
    }

    @Test
    fun uncertainPreAcceptanceFailuresRetainExactPreparedIdentity() = runBlocking {
        val retainedFailures = listOf(
            GoogleCalendarInboundApiException.Authentication() to
                GoogleCalendarImportOutcome.AUTH_REQUIRED,
            GoogleCalendarInboundApiException.Upstream() to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            GoogleCalendarInboundApiException.Unavailable() to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            GoogleCalendarInboundApiException.Http(429) to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            GoogleCalendarInboundApiException.Http(200) to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            GoogleCalendarInboundApiException.InvalidResponse() to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
            IOException("synthetic offline failure") to
                GoogleCalendarImportOutcome.RESPONSE_UNKNOWN,
        )

        for ((failure, expectedOutcome) in retainedFailures) {
            val journals = InMemoryGoogleImportJournalStore()
            val transport = FakeGoogleInboundTransport().apply {
                onRefresh = { _, _, _ -> throw failure }
            }
            val coordinator = coordinator(
                credentials = FakeGoogleImportCredentials(),
                transport = transport,
                journals = journals,
                pipeline = FakeGoogleImportPipeline(),
            )

            assertEquals(expectedOutcome, coordinator.refresh(ACCOUNT_A))
            val retained = journals.journals.single()
            assertEquals(REQUEST_ID, retained.requestId)
            assertFalse(retained.isAccepted)
            assertEquals(0, journals.rejectedRetirementCount)
            assertTrue(coordinator.hasCredentialRecoveryBlocker())
        }
    }

    @Test
    fun failedRejectionRetirementFailsClosedWithExactMarkerRetained() = runBlocking {
        val journals = InMemoryGoogleImportJournalStore().apply {
            failRejectedRetirement = true
        }
        val transport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, _, _ -> throw GoogleCalendarInboundApiException.Validation(400) }
        }
        val coordinator = coordinator(
            credentials = FakeGoogleImportCredentials(),
            transport = transport,
            journals = journals,
            pipeline = FakeGoogleImportPipeline(),
        )

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            coordinator.refresh(ACCOUNT_A),
        )
        assertEquals(GoogleCalendarImportPhase.RECOVERY_REQUIRED, coordinator.state.value.phase)
        assertEquals(1, journals.rejectedRetirementAttempts)
        assertEquals(REQUEST_ID, journals.journals.single().requestId)
        assertTrue(coordinator.hasCredentialRecoveryBlocker())
    }

    @Test
    fun completionRequiresIdleAndCompletedGenerationAtLeastAccepted() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val statuses = ArrayDeque(
            listOf(
                status(run(ACCOUNT_A, RemoteGoogleSyncRunState.IDLE, 8, 7)),
                status(run(ACCOUNT_A, RemoteGoogleSyncRunState.RUNNING, 8, 8)),
                status(run(ACCOUNT_A, RemoteGoogleSyncRunState.IDLE, 8, 8)),
            ),
        )
        val transport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, accountId, requestId -> accepted(accountId, requestId, 8) }
            onStatus = { _, _ -> statuses.removeFirst() }
        }
        val pipeline = FakeGoogleImportPipeline()
        val coordinator = coordinator(
            credentials,
            transport,
            journals,
            pipeline,
            retryPolicy = GoogleCalendarImportRetryPolicy(listOf(0, 0, 0)),
        )

        assertEquals(GoogleCalendarImportOutcome.COMPLETED, coordinator.refresh(ACCOUNT_A))
        assertEquals(3, transport.statusCalls)
        assertEquals(1, pipeline.inputs.size)
        assertEquals(GoogleCalendarImportPhase.COMPLETED, coordinator.state.value.phase)
        assertTrue(journals.journals.isEmpty())
    }

    @Test
    fun nonDurablePipelineReceiptRetainsAcceptedMarkerForLaterRecovery() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val firstTransport = completedTransport(generation = 5)
        val firstPipeline = FakeGoogleImportPipeline().apply {
            onPersist = { input -> receipt(input, durablyPersisted = false) }
        }
        val first = coordinator(credentials, firstTransport, journals, firstPipeline)

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            first.refresh(ACCOUNT_A),
        )
        val acceptedJournal = journals.journals.single()
        assertEquals(5L, acceptedJournal.acceptedRefreshGeneration)
        assertEquals(0, journals.removeCount)

        val recoveryTransport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, _, _ -> error("an accepted request must not be posted again") }
            onStatus = { _, accountId -> status(run(accountId, RemoteGoogleSyncRunState.IDLE, 5, 5)) }
        }
        val recoveryPipeline = FakeGoogleImportPipeline()
        val recovered = coordinator(credentials, recoveryTransport, journals, recoveryPipeline)

        assertEquals(
            GoogleCalendarImportOutcome.COMPLETED,
            recovered.recoverPending(ACCOUNT_A),
        )
        assertTrue(recoveryTransport.refreshRequestIds.isEmpty())
        assertEquals(1, recoveryPipeline.inputs.size)
        assertEquals(1, journals.removeCount)
        assertTrue(journals.journals.isEmpty())
    }

    @Test
    fun acceptanceSaveFailureStopsBeforePollingAndReplaysPreparedIdentity() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore().apply { failAcceptedSave = true }
        val transport = completedTransport(generation = 11)
        val pipeline = FakeGoogleImportPipeline()
        val first = coordinator(credentials, transport, journals, pipeline)

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            first.refresh(ACCOUNT_A),
        )
        assertEquals(0, transport.statusCalls)
        val prepared = journals.journals.single()
        assertFalse(prepared.isAccepted)

        journals.failAcceptedSave = false
        val recoveryTransport = completedTransport(generation = 11)
        val recovered = coordinator(
            credentials,
            recoveryTransport,
            journals,
            FakeGoogleImportPipeline(),
        )
        assertEquals(
            GoogleCalendarImportOutcome.COMPLETED,
            recovered.recoverPending(ACCOUNT_A),
        )
        assertEquals(UUID.fromString(prepared.requestId), recoveryTransport.refreshRequestIds.single())
        assertTrue(journals.journals.isEmpty())
    }

    @Test
    fun cancellationAfterDispatchKeepsPreparedRequestForExactRecovery() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val requestEntered = CompletableDeferred<Unit>()
        val neverCompletes = CompletableDeferred<RemoteGoogleSyncRefreshAccepted>()
        val transport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, _, _ ->
                requestEntered.complete(Unit)
                neverCompletes.await()
            }
        }
        val coordinator = coordinator(
            credentials,
            transport,
            journals,
            FakeGoogleImportPipeline(),
        )

        val refresh = async { coordinator.refresh(ACCOUNT_A) }
        withTimeout(3_000) { requestEntered.await() }
        refresh.cancelAndJoin()

        assertEquals(1, transport.refreshRequestIds.size)
        assertFalse(journals.journals.single().isAccepted)
        assertEquals(0, journals.removeCount)
        assertEquals(GoogleCalendarImportPhase.RECOVERY_REQUIRED, coordinator.state.value.phase)
    }

    @Test
    fun cancellationDuringCanonicalPersistenceKeepsAcceptedMarker() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val pipelineEntered = CompletableDeferred<Unit>()
        val neverCompletes = CompletableDeferred<GoogleCalendarImportPersistenceReceipt>()
        val pipeline = FakeGoogleImportPipeline().apply {
            onPersist = {
                pipelineEntered.complete(Unit)
                neverCompletes.await()
            }
        }
        val coordinator = coordinator(
            credentials,
            completedTransport(generation = 13),
            journals,
            pipeline,
        )

        val refresh = async { coordinator.refresh(ACCOUNT_A) }
        withTimeout(3_000) { pipelineEntered.await() }
        refresh.cancelAndJoin()

        assertEquals(13L, journals.journals.single().acceptedRefreshGeneration)
        assertEquals(0, journals.removeCount)
        assertEquals(GoogleCalendarImportPhase.RECOVERY_REQUIRED, coordinator.state.value.phase)
    }

    @Test
    fun privacyLockAfterIdleStatusValidationStopsCanonicalPipelineAndRetainsJournal() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val operationAllowed = AtomicBoolean(true)
        val pipeline = FakeGoogleImportPipeline()
        val statusValidated = CountDownLatch(1)
        val releaseStatusPublication = CountDownLatch(1)
        val blockExactlyOneLoad = AtomicBoolean(false)
        val transport = completedTransport(generation = 14).apply {
            onStatus = { _, accountId ->
                blockExactlyOneLoad.set(true)
                status(run(accountId, RemoteGoogleSyncRunState.IDLE, 14, 14))
            }
        }
        journals.beforeLoad = {
            if (blockExactlyOneLoad.compareAndSet(true, false)) {
                statusValidated.countDown()
                check(releaseStatusPublication.await(5, TimeUnit.SECONDS))
            }
        }
        val coordinator = coordinator(
            credentials = credentials,
            transport = transport,
            journals = journals,
            pipeline = pipeline,
            operationAllowed = operationAllowed::get,
        )

        val refresh = async(Dispatchers.Default) { coordinator.refresh(ACCOUNT_A) }
        try {
            assertTrue(statusValidated.await(5, TimeUnit.SECONDS))
            operationAllowed.set(false)
            coordinator.quarantineBindingState()
        } finally {
            releaseStatusPublication.countDown()
        }

        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            withTimeout(5_000) { refresh.await() },
        )
        assertTrue(pipeline.inputs.isEmpty())
        assertEquals(14L, journals.journals.single().acceptedRefreshGeneration)
        assertEquals(0, journals.removeCount)
        assertEquals(GoogleCalendarImportPhase.RECOVERY_REQUIRED, coordinator.state.value.phase)
    }

    @Test
    fun configurationConflictAndLostResponseReconcileOnlyThroughAuthoritativeGet() = runBlocking {
        for (failure in listOf<Exception>(
            GoogleCalendarInboundApiException.Conflict(),
            IOException("synthetic lost response"),
        )) {
            val credentials = FakeGoogleImportCredentials()
            val journals = InMemoryGoogleImportJournalStore()
            val original = collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                revision = 1,
            )
            val updated = collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                role = RemoteGoogleSyncRole.BLOCKING,
                selected = true,
                revision = 2,
            )
            val collectionReads = ArrayDeque(listOf(original, updated))
            val transport = FakeGoogleInboundTransport().apply {
                onConfigure = { _, _, _, _ -> throw failure }
                onCollections = { _, _ ->
                    RemoteGoogleCollections(listOf(collectionReads.removeFirst()))
                }
            }
            val coordinator = coordinator(
                credentials,
                transport,
                journals,
                FakeGoogleImportPipeline(),
            )
            assertEquals(
                GoogleImportCollectionsOutcome.LOADED,
                coordinator.loadCollections(ACCOUNT_A),
            )
            transport.collectionsCalls = 0

            assertEquals(
                GoogleImportConfigurationOutcome.RECONCILED,
                coordinator.configureCollection(
                    accountId = ACCOUNT_A,
                    collectionId = COLLECTION_A,
                    request = ConfigureGoogleCollectionRequest(
                        expectedRevision = 1,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        role = GoogleInboundCollectionRole.BLOCKING,
                    ),
                ),
            )
            assertEquals(1, transport.configureCalls)
            assertEquals(1, transport.collectionsCalls)
            assertEquals(2L, coordinator.state.value.accounts[ACCOUNT_A]
                ?.collections?.single()?.revision)
        }
    }

    @Test
    fun taskListsCanBeEnabledAndDisabledOnlyAsReadOnlySources() = runBlocking {
        val transport = FakeGoogleInboundTransport()
        val coordinator = coordinator(
            FakeGoogleImportCredentials(),
            transport,
            InMemoryGoogleImportJournalStore(),
            FakeGoogleImportPipeline(),
        )
        transport.onCollections = { _, _ ->
            RemoteGoogleCollections(
                listOf(
                    collection(
                        accountId = ACCOUNT_A,
                        id = COLLECTION_A,
                        kind = RemoteGoogleCollectionKind.TASK_LIST,
                        revision = 1,
                    ),
                ),
            )
        }
        assertEquals(
            GoogleImportCollectionsOutcome.LOADED,
            coordinator.loadCollections(ACCOUNT_A),
        )
        transport.collectionsCalls = 0

        assertEquals(
            GoogleImportConfigurationOutcome.CONFIGURED,
            coordinator.configureCollection(
                accountId = ACCOUNT_A,
                collectionId = COLLECTION_A,
                request = ConfigureGoogleCollectionRequest(
                    expectedRevision = 1,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    role = GoogleInboundCollectionRole.READ_ONLY,
                    visible = false,
                ),
            ),
        )
        val enabled = coordinator.state.value.accounts[ACCOUNT_A]?.collections?.single()
        assertEquals(RemoteGoogleCollectionKind.TASK_LIST, enabled?.kind)
        assertEquals(RemoteGoogleSyncRole.READ_ONLY, enabled?.syncRole)
        assertTrue(enabled?.selected == true)
        assertFalse(enabled?.visible == true)

        assertEquals(
            GoogleImportConfigurationOutcome.CONFIGURED,
            coordinator.configureCollection(
                accountId = ACCOUNT_A,
                collectionId = COLLECTION_A,
                request = ConfigureGoogleCollectionRequest(
                    expectedRevision = 2,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    role = GoogleInboundCollectionRole.OFF,
                ),
            ),
        )
        val disabled = coordinator.state.value.accounts[ACCOUNT_A]?.collections?.single()
        assertEquals(RemoteGoogleCollectionKind.TASK_LIST, disabled?.kind)
        assertFalse(disabled?.selected == true)
        assertFalse(disabled?.visible == true)
        assertEquals(2, transport.configureCalls)
        assertEquals(0, transport.collectionsCalls)
    }

    @Test
    fun taskListBlockingFailsLocallyWithoutMutationOrReconciliationTraffic() = runBlocking {
        val transport = FakeGoogleInboundTransport()
        val coordinator = coordinator(
            FakeGoogleImportCredentials(),
            transport,
            InMemoryGoogleImportJournalStore(),
            FakeGoogleImportPipeline(),
        )

        assertEquals(
            GoogleImportConfigurationOutcome.FAILED,
            coordinator.configureCollection(
                accountId = ACCOUNT_A,
                collectionId = COLLECTION_A,
                request = ConfigureGoogleCollectionRequest(
                    expectedRevision = 1,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    role = GoogleInboundCollectionRole.BLOCKING,
                ),
            ),
        )
        assertEquals(0, transport.configureCalls)
        assertEquals(0, transport.collectionsCalls)
        assertEquals(GoogleCalendarImportPhase.ERROR, coordinator.state.value.phase)
    }

    @Test
    fun configurationRequiresExactMutableCachedAuthoritativeSourceWithoutNetworkPreflight() =
        runBlocking {
            val cachedCases = listOf(
                "missing" to null,
                "wrong kind" to collection(
                    accountId = ACCOUNT_A,
                    id = COLLECTION_A,
                    kind = RemoteGoogleCollectionKind.CALENDAR,
                    revision = 1,
                ),
                "wrong revision" to collection(
                    accountId = ACCOUNT_A,
                    id = COLLECTION_A,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    revision = 2,
                ),
                "writable" to collection(
                    accountId = ACCOUNT_A,
                    id = COLLECTION_A,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    role = RemoteGoogleSyncRole.WRITABLE,
                    selected = true,
                    revision = 1,
                ),
                "provider deleted" to collection(
                    accountId = ACCOUNT_A,
                    id = COLLECTION_A,
                    kind = RemoteGoogleCollectionKind.TASK_LIST,
                    providerDeleted = true,
                    revision = 1,
                ),
            )

            cachedCases.forEach { (caseName, cached) ->
                val transport = FakeGoogleInboundTransport()
                val coordinator = coordinator(
                    FakeGoogleImportCredentials(),
                    transport,
                    InMemoryGoogleImportJournalStore(),
                    FakeGoogleImportPipeline(),
                )
                if (cached != null) {
                    transport.onCollections = { _, _ -> RemoteGoogleCollections(listOf(cached)) }
                    assertEquals(
                        "$caseName fixture must load",
                        GoogleImportCollectionsOutcome.LOADED,
                        coordinator.loadCollections(ACCOUNT_A),
                    )
                    transport.collectionsCalls = 0
                }
                transport.onCollections = { _, _ ->
                    error("$caseName must not trigger an authoritative network preflight")
                }

                assertEquals(
                    caseName,
                    GoogleImportConfigurationOutcome.FAILED,
                    coordinator.configureCollection(
                        accountId = ACCOUNT_A,
                        collectionId = COLLECTION_A,
                        request = ConfigureGoogleCollectionRequest(
                            expectedRevision = 1,
                            kind = RemoteGoogleCollectionKind.TASK_LIST,
                            role = GoogleInboundCollectionRole.READ_ONLY,
                        ),
                    ),
                )
                assertEquals("$caseName mutation calls", 0, transport.configureCalls)
                assertEquals("$caseName reconciliation calls", 0, transport.collectionsCalls)
                assertEquals(GoogleCalendarImportPhase.ERROR, coordinator.state.value.phase)
            }
        }

    @Test
    fun mismatchedTaskMutationResponseReconcilesOnlyToAnExactAuthoritativeKindAndPolicy() =
        runBlocking {
            val policy = RemoteGoogleCalendarPolicy.inboundDefault().copy(
                tentative = RemoteGoogleEventDisposition.IGNORE,
            )
            val authoritative = collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                selected = true,
                visible = false,
                revision = 4,
                policy = policy,
            )
            val cached = collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                revision = 1,
            )
            val collectionReads = ArrayDeque(listOf(cached, authoritative))
            val transport = FakeGoogleInboundTransport().apply {
                onConfigure = { _, accountId, collectionId, request ->
                    collection(
                        accountId = accountId,
                        id = collectionId,
                        kind = RemoteGoogleCollectionKind.CALENDAR,
                        selected = true,
                        visible = request.visible,
                        revision = request.expectedRevision + 1,
                        policy = request.calendarPolicy,
                    )
                }
                onCollections = { _, _ ->
                    RemoteGoogleCollections(listOf(collectionReads.removeFirst()))
                }
            }
            val coordinator = coordinator(
                FakeGoogleImportCredentials(),
                transport,
                InMemoryGoogleImportJournalStore(),
                FakeGoogleImportPipeline(),
            )
            assertEquals(
                GoogleImportCollectionsOutcome.LOADED,
                coordinator.loadCollections(ACCOUNT_A),
            )
            transport.collectionsCalls = 0

            assertEquals(
                GoogleImportConfigurationOutcome.RECONCILED,
                coordinator.configureCollection(
                    accountId = ACCOUNT_A,
                    collectionId = COLLECTION_A,
                    request = ConfigureGoogleCollectionRequest(
                        expectedRevision = 1,
                        kind = RemoteGoogleCollectionKind.TASK_LIST,
                        role = GoogleInboundCollectionRole.READ_ONLY,
                        visible = false,
                        calendarPolicy = policy,
                    ),
                ),
            )
            val installed = coordinator.state.value.accounts[ACCOUNT_A]?.collections?.single()
            assertEquals(RemoteGoogleCollectionKind.TASK_LIST, installed?.kind)
            assertEquals(4L, installed?.revision)
            assertEquals(policy, installed?.calendarPolicy)
            assertEquals(1, transport.configureCalls)
            assertEquals(1, transport.collectionsCalls)
        }

    @Test
    fun ambiguousTaskConfigurationRejectsWrongKindPolicyAndUnadvancedRevision() = runBlocking {
        val policy = RemoteGoogleCalendarPolicy.inboundDefault().copy(
            tentative = RemoteGoogleEventDisposition.IGNORE,
        )
        val invalidAuthoritativeCandidates = listOf(
            collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.CALENDAR,
                selected = true,
                revision = 2,
                policy = policy,
            ),
            collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                selected = true,
                revision = 2,
                policy = RemoteGoogleCalendarPolicy.inboundDefault(),
            ),
            collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                selected = true,
                revision = 1,
                policy = policy,
            ),
        )

        invalidAuthoritativeCandidates.forEach { candidate ->
            val cached = collection(
                accountId = ACCOUNT_A,
                id = COLLECTION_A,
                kind = RemoteGoogleCollectionKind.TASK_LIST,
                revision = 1,
            )
            val collectionReads = ArrayDeque(listOf(cached, candidate))
            val transport = FakeGoogleInboundTransport().apply {
                onConfigure = { _, _, _, _ -> throw IOException("synthetic lost response") }
                onCollections = { _, _ ->
                    RemoteGoogleCollections(listOf(collectionReads.removeFirst()))
                }
            }
            val coordinator = coordinator(
                FakeGoogleImportCredentials(),
                transport,
                InMemoryGoogleImportJournalStore(),
                FakeGoogleImportPipeline(),
            )
            assertEquals(
                GoogleImportCollectionsOutcome.LOADED,
                coordinator.loadCollections(ACCOUNT_A),
            )
            transport.collectionsCalls = 0

            assertEquals(
                GoogleImportConfigurationOutcome.OFFLINE,
                coordinator.configureCollection(
                    accountId = ACCOUNT_A,
                    collectionId = COLLECTION_A,
                    request = ConfigureGoogleCollectionRequest(
                        expectedRevision = 1,
                        kind = RemoteGoogleCollectionKind.TASK_LIST,
                        role = GoogleInboundCollectionRole.READ_ONLY,
                        calendarPolicy = policy,
                    ),
                ),
            )
            assertEquals(1, transport.configureCalls)
            assertEquals(1, transport.collectionsCalls)
            val retained = coordinator.state.value.accounts[ACCOUNT_A]?.collections?.single()
            assertEquals(RemoteGoogleCollectionKind.TASK_LIST, retained?.kind)
            assertEquals(1L, retained?.revision)
        }
    }

    @Test
    fun boundedStatusBackoffStopsWithoutClearingAcceptedRecovery() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val journals = InMemoryGoogleImportJournalStore()
        val transport = FakeGoogleInboundTransport().apply {
            onRefresh = { _, accountId, requestId -> accepted(accountId, requestId, 3) }
            onStatus = { _, _ -> throw GoogleCalendarInboundApiException.Unavailable() }
        }
        val coordinator = coordinator(
            credentials,
            transport,
            journals,
            FakeGoogleImportPipeline(),
            retryPolicy = GoogleCalendarImportRetryPolicy(listOf(0, 0, 0)),
        )

        assertEquals(GoogleCalendarImportOutcome.PENDING, coordinator.refresh(ACCOUNT_A))
        assertEquals(3, transport.statusCalls)
        assertEquals(3L, journals.journals.single().acceptedRefreshGeneration)
        assertEquals(0, journals.removeCount)
        assertFalse(coordinator.state.value.isBusy)
    }

    @Test
    fun multiAccountDiscoveryPreservesWritableInventoryButRedactsDiagnostics() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val writable = collection(
            accountId = ACCOUNT_A,
            id = COLLECTION_A,
            displayName = "Private work calendar",
            role = RemoteGoogleSyncRole.WRITABLE,
            providerDeleted = true,
            policy = RemoteGoogleCalendarPolicy.inboundDefault().copy(publishAllDay = true),
        )
        val second = collection(
            accountId = ACCOUNT_B,
            id = COLLECTION_B,
            displayName = "Private home calendar",
        )
        val transport = FakeGoogleInboundTransport().apply {
            onCollections = { _, accountId ->
                RemoteGoogleCollections(if (accountId == ACCOUNT_A) listOf(writable) else emptyList())
            }
            onDiscover = { _, accountId ->
                RemoteGoogleCollections(if (accountId == ACCOUNT_B) listOf(second) else emptyList())
            }
        }
        val coordinator = coordinator(
            credentials,
            transport,
            InMemoryGoogleImportJournalStore(),
            FakeGoogleImportPipeline(),
        )

        assertEquals(GoogleImportCollectionsOutcome.LOADED, coordinator.loadCollections(ACCOUNT_A))
        assertEquals(
            GoogleImportCollectionsOutcome.LOADED,
            coordinator.discoverCollections(ACCOUNT_B),
        )
        val state = coordinator.state.value
        assertEquals(setOf(ACCOUNT_A, ACCOUNT_B), state.accounts.keys)
        assertEquals(RemoteGoogleSyncRole.WRITABLE, state.accounts[ACCOUNT_A]
            ?.collections?.single()?.syncRole)
        assertEquals(
            "owner",
            state.accounts[ACCOUNT_A]?.collections?.single()?.providerAccessRole,
        )
        assertTrue(state.accounts[ACCOUNT_A]?.collections?.single()?.providerDeleted == true)
        val diagnostic = state.toString() + writable.toStateForDiagnostic().toString()
        assertFalse(diagnostic.contains(ACCOUNT_A))
        assertFalse(diagnostic.contains(CONFIGURATION_A))
        assertFalse(diagnostic.contains("Private work calendar"))
        assertTrue(diagnostic.contains("<redacted>"))
    }

    @Test
    fun quarantinedPresentationExposesPendingCountWithoutAccountRoutingOrCachedLabels() =
        runBlocking {
            val journals = InMemoryGoogleImportJournalStore().apply {
                this.journals += preparedJournal()
            }
            val transport = FakeGoogleInboundTransport().apply {
                onCollections = { _, accountId ->
                    RemoteGoogleCollections(
                        listOf(
                            collection(
                                accountId = accountId,
                                id = COLLECTION_A,
                                displayName = "Private quarantine calendar",
                            ),
                        ),
                    )
                }
            }
            val coordinator = coordinator(
                FakeGoogleImportCredentials(),
                transport,
                journals,
                FakeGoogleImportPipeline(),
            )

            assertEquals(1, coordinator.state.value.pendingRecoveryCount)
            assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
            assertTrue(coordinator.state.value.accounts.isEmpty())
            assertNull(coordinator.state.value.activeAccountId)

            assertEquals(
                GoogleImportCollectionsOutcome.LOADED,
                coordinator.loadCollections(ACCOUNT_A),
            )
            assertEquals(setOf(ACCOUNT_A), coordinator.state.value.pendingRecoveryAccountIds)
            assertEquals(
                "Private quarantine calendar",
                coordinator.state.value.accounts[ACCOUNT_A]?.collections?.single()?.displayName,
            )

            coordinator.quarantineBindingState()

            assertEquals(1, coordinator.state.value.pendingRecoveryCount)
            assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
            assertTrue(coordinator.state.value.accounts.isEmpty())
            assertNull(coordinator.state.value.activeAccountId)
        }

    @Test
    fun quarantineCannotBeOverwrittenByStatePublicationAlreadyWaitingOnJournalIo() = runBlocking {
        val journals = InMemoryGoogleImportJournalStore().apply {
            this.journals += preparedJournal()
        }
        val transport = FakeGoogleInboundTransport().apply {
            onCollections = { _, accountId ->
                RemoteGoogleCollections(
                    listOf(
                        collection(
                            accountId = accountId,
                            id = COLLECTION_A,
                            displayName = "Private stale calendar",
                        ),
                    ),
                )
            }
        }
        val coordinator = coordinator(
            FakeGoogleImportCredentials(),
            transport,
            journals,
            FakeGoogleImportPipeline(),
        )
        assertEquals(
            GoogleImportCollectionsOutcome.LOADED,
            coordinator.loadCollections(ACCOUNT_A),
        )

        val stalePublicationReachedJournal = CountDownLatch(1)
        val releaseStalePublication = CountDownLatch(1)
        val blockExactlyOneLoad = AtomicBoolean(true)
        journals.beforeLoad = {
            if (blockExactlyOneLoad.compareAndSet(true, false)) {
                stalePublicationReachedJournal.countDown()
                check(releaseStalePublication.await(5, TimeUnit.SECONDS))
            }
        }
        val staleOperation = async(Dispatchers.Default) {
            coordinator.discoverCollections(ACCOUNT_A)
        }

        try {
            assertTrue(stalePublicationReachedJournal.await(5, TimeUnit.SECONDS))
            coordinator.quarantineBindingState()

            assertEquals(1, coordinator.state.value.pendingRecoveryCount)
            assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
            assertTrue(coordinator.state.value.accounts.isEmpty())
            assertNull(coordinator.state.value.activeAccountId)
        } finally {
            releaseStalePublication.countDown()
        }

        assertEquals(
            GoogleImportCollectionsOutcome.RECOVERY_REQUIRED,
            withTimeout(5_000) { staleOperation.await() },
        )
        assertEquals(1, coordinator.state.value.pendingRecoveryCount)
        assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
        assertTrue(coordinator.state.value.accounts.isEmpty())
        assertNull(coordinator.state.value.activeAccountId)
    }

    @Test
    fun disabledOperationGateSuppressesPublicationBeforeQuarantineAdvancesGeneration() =
        runBlocking {
            val journals = InMemoryGoogleImportJournalStore()
            val operationAllowed = AtomicBoolean(true)
            val transport = FakeGoogleInboundTransport().apply {
                onCollections = { _, accountId ->
                    RemoteGoogleCollections(
                        listOf(
                            collection(
                                accountId = accountId,
                                id = COLLECTION_A,
                                displayName = "Private pre-quarantine calendar",
                            ),
                        ),
                    )
                }
            }
            val coordinator = coordinator(
                credentials = FakeGoogleImportCredentials(),
                transport = transport,
                journals = journals,
                pipeline = FakeGoogleImportPipeline(),
                operationAllowed = operationAllowed::get,
            )
            val publicationReachedJournal = CountDownLatch(1)
            val releasePublication = CountDownLatch(1)
            val blockExactlyOneLoad = AtomicBoolean(true)
            journals.beforeLoad = {
                if (blockExactlyOneLoad.compareAndSet(true, false)) {
                    publicationReachedJournal.countDown()
                    check(releasePublication.await(5, TimeUnit.SECONDS))
                }
            }
            val operation = async(Dispatchers.Default) {
                coordinator.loadCollections(ACCOUNT_A)
            }

            try {
                assertTrue(publicationReachedJournal.await(5, TimeUnit.SECONDS))
                operationAllowed.set(false)
            } finally {
                releasePublication.countDown()
            }

            assertEquals(
                GoogleImportCollectionsOutcome.LOADED,
                withTimeout(5_000) { operation.await() },
            )
            assertEquals(GoogleCalendarImportPhase.READY, coordinator.state.value.phase)
            assertTrue(coordinator.state.value.accounts.isEmpty())
            assertNull(coordinator.state.value.activeAccountId)

            coordinator.quarantineBindingState()
            assertTrue(coordinator.state.value.accounts.isEmpty())
            assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
            assertNull(coordinator.state.value.activeAccountId)
        }

    @Test
    fun foreignCredentialJournalBlocksOrdinaryUseUntilConfirmedDestruction() = runBlocking {
        val credentials = FakeGoogleImportCredentials(configurationId = CONFIGURATION_B)
        val journals = InMemoryGoogleImportJournalStore().apply {
            journals += preparedJournal(configurationId = CONFIGURATION_A)
        }
        val transport = FakeGoogleInboundTransport()
        val coordinator = coordinator(
            credentials,
            transport,
            journals,
            FakeGoogleImportPipeline(),
        )

        assertTrue(coordinator.hasCredentialRecoveryBlocker())
        assertEquals(
            GoogleCalendarImportOutcome.RECOVERY_REQUIRED,
            coordinator.refresh(ACCOUNT_A),
        )
        coordinator.quarantineBindingState()
        assertEquals(1, journals.journals.size)
        assertTrue(coordinator.hasCredentialRecoveryBlocker())
        assertTrue(coordinator.abandonPendingForConfirmedLocalDestruction())
        assertTrue(journals.journals.isEmpty())
        assertFalse(coordinator.hasCredentialRecoveryBlocker())
        assertTrue(transport.refreshRequestIds.isEmpty())
    }

    @Test
    fun authoritativeFailedRunRestartsWithNewDurableIdentityOnlyOnUserRefresh() = runBlocking {
        val credentials = FakeGoogleImportCredentials()
        val acceptedJournal = preparedJournal().recordingAcceptance(4, NOW.toEpochMilli())
        val journals = InMemoryGoogleImportJournalStore().apply {
            this.journals += acceptedJournal
        }
        val statusQueue = ArrayDeque(
            listOf(
                status(run(ACCOUNT_A, RemoteGoogleSyncRunState.FAILED, 4, 0)),
                status(run(ACCOUNT_A, RemoteGoogleSyncRunState.IDLE, 5, 5)),
            ),
        )
        val transport = FakeGoogleInboundTransport().apply {
            onStatus = { _, _ -> statusQueue.removeFirst() }
            onRefresh = { _, accountId, requestId -> accepted(accountId, requestId, 5) }
        }
        val coordinator = coordinator(
            credentials = credentials,
            transport = transport,
            journals = journals,
            pipeline = FakeGoogleImportPipeline(),
            newRequestId = { UUID.fromString(REQUEST_ID_B) },
        )

        assertEquals(GoogleCalendarImportOutcome.COMPLETED, coordinator.refresh(ACCOUNT_A))
        assertEquals(listOf(UUID.fromString(REQUEST_ID_B)), transport.refreshRequestIds)
        assertTrue(journals.journals.isEmpty())
    }

    @Test
    fun sourceConfigurationIsFencedUntilSavedImportFinishes() = runBlocking {
        val journals = InMemoryGoogleImportJournalStore().apply {
            this.journals += preparedJournal()
        }
        val transport = FakeGoogleInboundTransport()
        val coordinator = coordinator(
            FakeGoogleImportCredentials(),
            transport,
            journals,
            FakeGoogleImportPipeline(),
        )

        assertTrue(coordinator.state.value.pendingRecoveryAccountIds.isEmpty())
        assertEquals(
            GoogleImportConfigurationOutcome.RECOVERY_REQUIRED,
            coordinator.configureCollection(
                accountId = ACCOUNT_A,
                collectionId = COLLECTION_A,
                request = ConfigureGoogleCollectionRequest(
                    expectedRevision = 1,
                    kind = RemoteGoogleCollectionKind.CALENDAR,
                    role = GoogleInboundCollectionRole.BLOCKING,
                ),
            ),
        )
        assertEquals(setOf(ACCOUNT_A), coordinator.state.value.pendingRecoveryAccountIds)
        assertEquals(0, transport.configureCalls)
        assertEquals(REQUEST_ID, journals.journals.single().requestId)
    }

    private fun coordinator(
        credentials: FakeGoogleImportCredentials,
        transport: FakeGoogleInboundTransport,
        journals: InMemoryGoogleImportJournalStore,
        pipeline: FakeGoogleImportPipeline,
        retryPolicy: GoogleCalendarImportRetryPolicy = GoogleCalendarImportRetryPolicy(listOf(0)),
        newRequestId: () -> UUID = { UUID.fromString(REQUEST_ID) },
        operationAllowed: () -> Boolean = { true },
        importAllowed: () -> Boolean = { true },
    ): GoogleCalendarImportCoordinator = GoogleCalendarImportCoordinator(
        credentialStore = credentials,
        transport = transport,
        journalStore = journals,
        completionPipeline = pipeline,
        retryPolicy = retryPolicy,
        nowEpochMillis = { NOW.toEpochMilli() },
        newRequestId = newRequestId,
        sleep = {},
        operationAllowed = operationAllowed,
        importAllowed = importAllowed,
    )

    private fun completedTransport(generation: Long): FakeGoogleInboundTransport =
        FakeGoogleInboundTransport().apply {
            onRefresh = { _, accountId, requestId -> accepted(accountId, requestId, generation) }
            onStatus = { _, accountId ->
                status(run(accountId, RemoteGoogleSyncRunState.IDLE, generation, generation))
            }
        }

    companion object {
        val NOW: Instant = Instant.parse("2026-09-02T12:00:00Z")
        const val API_BASE_URL = "https://dayweave.example/gateway/"
        const val CONFIGURATION_A = "11111111-1111-4111-8111-111111111111"
        const val CONFIGURATION_B = "22222222-2222-4222-8222-222222222222"
        const val ACCOUNT_A = "33333333-3333-4333-8333-333333333333"
        const val ACCOUNT_B = "44444444-4444-4444-8444-444444444444"
        const val COLLECTION_A = "55555555-5555-4555-8555-555555555555"
        const val COLLECTION_B = "66666666-6666-4666-8666-666666666666"
        const val REQUEST_ID = "77777777-7777-4777-8777-777777777777"
        const val REQUEST_ID_B = "88888888-8888-4888-8888-888888888888"

        fun accepted(
            accountId: String,
            requestId: UUID,
            generation: Long,
        ): RemoteGoogleSyncRefreshAccepted = RemoteGoogleSyncRefreshAccepted(
            accountId = accountId,
            requestId = requestId.toString(),
            refreshGeneration = generation,
            requestedAt = NOW.toString(),
        )

        fun status(run: RemoteGoogleSyncRunStatus?): RemoteGoogleSyncStatus =
            RemoteGoogleSyncStatus(
                run = run,
                importConflicts = 0,
                pendingOutbound = 0,
                conflictedOutbound = 0,
                failedOutbound = 0,
                lastOutboundErrorCode = null,
                lastOutboundErrorAt = null,
                nextOutboundAttemptAt = null,
            )

        fun run(
            accountId: String,
            state: RemoteGoogleSyncRunState,
            refreshGeneration: Long,
            completedGeneration: Long,
        ): RemoteGoogleSyncRunStatus = RemoteGoogleSyncRunStatus(
            accountId = accountId,
            state = state,
            requestedAt = NOW.minusSeconds(10).toString(),
            startedAt = NOW.minusSeconds(9).toString(),
            completedAt = if (state == RemoteGoogleSyncRunState.IDLE) NOW.minusSeconds(1).toString() else null,
            nextAttemptAt = NOW.plusSeconds(60).toString(),
            consecutiveFailures = 0,
            lastErrorCode = null,
            lastErrorAt = null,
            importedCount = 1,
            updatedCount = 2,
            deletedCount = 3,
            conflictCount = 0,
            rejectedCount = 0,
            refreshGeneration = refreshGeneration,
            claimedRefreshGeneration = refreshGeneration,
            completedRefreshGeneration = completedGeneration,
            revision = refreshGeneration.coerceAtLeast(1),
        )

        fun collection(
            accountId: String,
            id: String,
            displayName: String = "Calendar",
            kind: RemoteGoogleCollectionKind = RemoteGoogleCollectionKind.CALENDAR,
            role: RemoteGoogleSyncRole = RemoteGoogleSyncRole.READ_ONLY,
            selected: Boolean = role != RemoteGoogleSyncRole.READ_ONLY,
            visible: Boolean = true,
            providerDeleted: Boolean = false,
            revision: Long = 1,
            policy: RemoteGoogleCalendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
        ): RemoteGoogleSyncCollection = RemoteGoogleSyncCollection(
            id = id,
            accountId = accountId,
            kind = kind,
            remoteCollectionId = "remote-$id",
            displayName = displayName,
            providerAccessRole = if (role == RemoteGoogleSyncRole.WRITABLE) "owner" else "reader",
            providerPrimary = true,
            providerSelected = true,
            providerHidden = false,
            providerDeleted = providerDeleted,
            selected = selected,
            visible = visible,
            syncRole = role,
            calendarPolicy = policy,
            revision = revision,
            discoveredAt = NOW.minusSeconds(300).toString(),
            configuredAt = NOW.minusSeconds(200).toString(),
            lastImportAt = NOW.minusSeconds(100).toString(),
            planningProjectionState = RemoteGoogleCalendarProjectionState.COMPLETE,
            planningGeneration = 1,
            planningCollectionRevision = revision,
            planningWindowStart = NOW.minusSeconds(3_600).toString(),
            planningWindowEnd = NOW.plusSeconds(3_600).toString(),
            planningWindowRefreshedAt = NOW.minusSeconds(100).toString(),
            createdAt = NOW.minusSeconds(400).toString(),
            updatedAt = NOW.minusSeconds(100).toString(),
        )

        fun preparedJournal(
            configurationId: String = CONFIGURATION_A,
        ): GoogleCalendarImportJournal = GoogleCalendarImportJournal(
            configurationId = configurationId,
            apiBaseUrl = API_BASE_URL,
            accountId = ACCOUNT_A,
            requestId = REQUEST_ID,
            createdAtEpochMillis = NOW.toEpochMilli(),
        )

        fun receipt(
            input: GoogleCalendarImportCompletionInput,
            durablyPersisted: Boolean = true,
        ): GoogleCalendarImportPersistenceReceipt = GoogleCalendarImportPersistenceReceipt(
            configurationId = input.configurationId,
            apiBaseUrl = input.apiBaseUrl,
            accountId = input.accountId,
            completedRefreshGeneration = input.acceptedRefreshGeneration,
            durablyPersisted = durablyPersisted,
        )
    }
}

private class FakeGoogleImportCredentials(
    var configurationId: String = GoogleCalendarImportCoordinatorTest.CONFIGURATION_A,
) : ApiCredentialStore {
    var enabled: Boolean = true
    var baseUrl: String = GoogleCalendarImportCoordinatorTest.API_BASE_URL

    override fun snapshot(): ApiConnectionSnapshot = ApiConnectionSnapshot(
        baseUrl = baseUrl,
        hasBearerToken = enabled,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        if (enabled) {
            AuthenticatedApiConfiguration.createBound(
                baseUrl = baseUrl,
                bearerToken = "synthetic-google-import-token",
                configurationId = configurationId,
            )
        } else {
            null
        }

    override fun update(baseUrl: String, bearerToken: String?) {
        this.baseUrl = baseUrl
    }

    override fun clear() {
        enabled = false
    }

    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class InMemoryGoogleImportJournalStore : GoogleCalendarImportJournalStore {
    val journals = mutableListOf<GoogleCalendarImportJournal>()
    var beforeLoad: (() -> Unit)? = null
    var corrupt = false
    var failAcceptedSave = false
    var failRemove = false
    var failRejectedRetirement = false
    var failAbandon = false
    var removeCount = 0
    var rejectedRetirementAttempts = 0
    var rejectedRetirementCount = 0
    var afterSave: ((GoogleCalendarImportJournal) -> Unit)? = null

    override fun load(nowEpochMillis: Long): GoogleCalendarImportJournalLoadResult {
        beforeLoad?.invoke()
        return if (corrupt) {
            GoogleCalendarImportJournalLoadResult.Corrupt
        } else {
            GoogleCalendarImportJournalLoadResult.Loaded(journals.toList())
        }
    }

    override fun save(
        journal: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean {
        if (corrupt || failAcceptedSave && journal.isAccepted || !journal.isValidAt(nowEpochMillis)) {
            return false
        }
        val index = journals.indexOfFirst {
            it.configurationId == journal.configurationId && it.accountId == journal.accountId
        }
        if (index < 0) {
            journals += journal
            afterSave?.invoke(journal)
            return true
        }
        val existing = journals[index]
        if (
            existing.requestId != journal.requestId ||
            existing.createdAtEpochMillis != journal.createdAtEpochMillis ||
            existing.apiBaseUrl != journal.apiBaseUrl ||
            existing.isAccepted && existing != journal
        ) {
            return false
        }
        journals[index] = journal
        afterSave?.invoke(journal)
        return true
    }

    override fun removeExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean {
        if (corrupt || failRemove) return false
        val removed = journals.remove(expected)
        if (removed) removeCount += 1
        return removed
    }

    override fun retireRejectedPreparedExact(
        expected: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean {
        rejectedRetirementAttempts += 1
        if (
            corrupt || failRejectedRetirement || expected.isAccepted ||
            !expected.isValidAt(nowEpochMillis)
        ) {
            return false
        }
        val removed = journals.remove(expected)
        if (removed) rejectedRetirementCount += 1
        return removed
    }

    override fun restartAcceptedExact(
        expected: GoogleCalendarImportJournal,
        replacement: GoogleCalendarImportJournal,
        nowEpochMillis: Long,
    ): Boolean {
        if (
            corrupt || !expected.isAccepted || replacement.isAccepted ||
            replacement.configurationId != expected.configurationId ||
            replacement.apiBaseUrl != expected.apiBaseUrl ||
            replacement.accountId != expected.accountId ||
            replacement.requestId == expected.requestId ||
            !replacement.isValidAt(nowEpochMillis)
        ) return false
        val index = journals.indexOf(expected)
        if (index < 0) return false
        journals[index] = replacement
        return true
    }

    override fun abandonAllForConfirmedLocalDestruction(nowEpochMillis: Long): Boolean {
        if (failAbandon) return false
        corrupt = false
        journals.clear()
        return true
    }
}

private class FakeGoogleInboundTransport : GoogleCalendarInboundTransport {
    var collectionsCalls = 0
    var discoverCalls = 0
    var configureCalls = 0
    var statusCalls = 0
    val refreshRequestIds = mutableListOf<UUID>()

    var onCollections: suspend (
        AuthenticatedApiConfiguration,
        String,
    ) -> RemoteGoogleCollections = { _, _ -> RemoteGoogleCollections(emptyList()) }
    var onDiscover: suspend (
        AuthenticatedApiConfiguration,
        String,
    ) -> RemoteGoogleCollections = { _, _ -> RemoteGoogleCollections(emptyList()) }
    var onConfigure: suspend (
        AuthenticatedApiConfiguration,
        String,
        String,
        ConfigureGoogleCollectionRequest,
    ) -> RemoteGoogleSyncCollection = { _, accountId, collectionId, request ->
        GoogleCalendarImportCoordinatorTest.collection(
            accountId = accountId,
            id = collectionId,
            kind = request.kind,
            role = when (request.role) {
                GoogleInboundCollectionRole.BLOCKING -> RemoteGoogleSyncRole.BLOCKING
                GoogleInboundCollectionRole.OFF,
                GoogleInboundCollectionRole.READ_ONLY,
                -> RemoteGoogleSyncRole.READ_ONLY
            },
            selected = request.role != GoogleInboundCollectionRole.OFF,
            visible = request.role != GoogleInboundCollectionRole.OFF && request.visible,
            revision = request.expectedRevision + 1,
            policy = request.calendarPolicy,
        )
    }
    var onStatus: suspend (
        AuthenticatedApiConfiguration,
        String,
    ) -> RemoteGoogleSyncStatus = { _, _ -> GoogleCalendarImportCoordinatorTest.status(null) }
    var onRefresh: suspend (
        AuthenticatedApiConfiguration,
        String,
        UUID,
    ) -> RemoteGoogleSyncRefreshAccepted = { _, accountId, requestId ->
        GoogleCalendarImportCoordinatorTest.accepted(accountId, requestId, 1)
    }

    override suspend fun collections(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections {
        collectionsCalls += 1
        return onCollections(configuration, accountId)
    }

    override suspend fun discover(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleCollections {
        discoverCalls += 1
        return onDiscover(configuration, accountId)
    }

    override suspend fun configure(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        request: ConfigureGoogleCollectionRequest,
    ): RemoteGoogleSyncCollection {
        configureCalls += 1
        return onConfigure(configuration, accountId, collectionId, request)
    }

    override suspend fun syncStatus(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
    ): RemoteGoogleSyncStatus {
        statusCalls += 1
        return onStatus(configuration, accountId)
    }

    override suspend fun refresh(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        requestId: UUID,
    ): RemoteGoogleSyncRefreshAccepted {
        refreshRequestIds += requestId
        return onRefresh(configuration, accountId, requestId)
    }
}

private class FakeGoogleImportPipeline : GoogleCalendarImportCompletionPipeline {
    val inputs = mutableListOf<GoogleCalendarImportCompletionInput>()
    var onPersist: suspend (
        GoogleCalendarImportCompletionInput,
    ) -> GoogleCalendarImportPersistenceReceipt = {
        GoogleCalendarImportCoordinatorTest.receipt(it)
    }

    override suspend fun persistCanonicalRefreshCompositionAndPublication(
        input: GoogleCalendarImportCompletionInput,
    ): GoogleCalendarImportPersistenceReceipt {
        inputs += input
        return onPersist(input)
    }
}

private fun RemoteGoogleSyncCollection.toStateForDiagnostic(): GoogleImportCollectionState =
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
    )
