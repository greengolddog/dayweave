package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalEventTimingDraft
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.GoogleCalendarOutboundApprovalCapability
import com.greengolddog.dayweave.model.GoogleCalendarOutboundJournal
import com.greengolddog.dayweave.model.GoogleCalendarOutboundStage
import com.greengolddog.dayweave.model.GoogleCalendarOutboundTarget
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleCalendarOutboundApiException
import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.network.GoogleCalendarOutboundTransport
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleOutboundAccepted
import com.greengolddog.dayweave.network.RemoteGoogleOutboundApproval
import com.greengolddog.dayweave.network.RemoteGoogleOutboundPreview
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import com.greengolddog.dayweave.state.PlannerStore
import java.io.IOException
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleCalendarOutboundCoordinatorTest {
    @Test
    fun preparePersistsExactIntentBeforePreviewAndPersistsPreviewBeforeConfirmation() =
        runBlocking {
            val store = PlannerStore(outboundState())
            val transport = FakeGoogleOutboundTransport().apply {
                onPreview = { call ->
                    val persisted = requireNotNull(
                        store.durableState.value?.pendingGoogleCalendarOutbound,
                    )
                    assertEquals(GoogleCalendarOutboundStage.INTENT, persisted.stage)
                    assertEquals(ACCOUNT_ID, persisted.accountId)
                    assertEquals(COLLECTION_ID, persisted.collectionId)
                    assertEquals(ITEM_ID, persisted.itemId)
                    assertEquals(ITEM_REVISION, persisted.expectedItemRevision)
                    assertEquals(API_BASE_URL, persisted.apiBaseUrl)
                    assertEquals(CONFIGURATION_A, persisted.configurationId)
                    assertEquals(ACCOUNT_ID, call.accountId)
                    assertEquals(COLLECTION_ID, call.collectionId)
                    assertEquals(ITEM_ID, call.itemId)
                    assertEquals(ITEM_REVISION, call.expectedItemRevision)
                    validPreview()
                }
            }
            val coordinator = coordinator(store = store, transport = transport)

            val targets = coordinator.targetsFor(ITEM_ID)
            assertEquals(1, targets.size)
            assertEquals(TARGET, targets.single().target)
            assertEquals(
                GoogleCalendarOutboundOutcome.PREVIEW_READY,
                coordinator.preparePreview(ITEM_ID, TARGET),
            )

            val persisted = requireNotNull(store.durableState.value?.pendingGoogleCalendarOutbound)
            assertEquals(GoogleCalendarOutboundStage.PREVIEWED, persisted.stage)
            assertEquals(validPreview().previewHash, persisted.preview?.previewHash)
            assertEquals(GoogleCalendarOutboundPhase.AWAITING_APPROVAL, coordinator.state.value.phase)
            assertEquals(persisted.preview, coordinator.state.value.preview)
            assertNotNull(coordinator.approvalConfirmation())
            assertEquals(
                "Private Gmail · Private calendar",
                coordinator.pendingDestinationOption()?.displayName,
            )
            assertFalse(coordinator.resetPresentationWithoutRecovery())
            assertEquals(1, transport.previewCalls.size)
            assertTrue(transport.approvalCalls.isEmpty())
            assertTrue(transport.enqueueCalls.isEmpty())
        }

    @Test
    fun prepareRequiresCurrentEligibleCandidateAndExactAuthoritativeWritableTarget() = runBlocking {
        val invalidCandidateStore = PlannerStore(
            outboundState(item = canonicalEvent(tentative = true)),
        )
        val invalidCandidateTransport = FakeGoogleOutboundTransport()
        val invalidCandidateCoordinator = coordinator(
            store = invalidCandidateStore,
            transport = invalidCandidateTransport,
        )

        assertTrue(invalidCandidateCoordinator.targetsFor(ITEM_ID).isEmpty())
        assertEquals(
            GoogleCalendarOutboundOutcome.FAILED,
            invalidCandidateCoordinator.preparePreview(ITEM_ID, TARGET),
        )
        assertNull(invalidCandidateStore.state.value.pendingGoogleCalendarOutbound)
        assertTrue(invalidCandidateTransport.previewCalls.isEmpty())

        val unsafeCases = listOf(
            outboundContext(
                account = accountSummary().copy(hasCalendarWriteScope = false),
            ),
            outboundContext(
                collection = writableCollection().copy(
                    providerAccessRole = "reader",
                    syncRole = RemoteGoogleSyncRole.READ_ONLY,
                ),
            ),
            outboundContext(
                collection = writableCollection().copy(providerDeleted = true),
            ),
            outboundContext(
                collection = writableCollection().copy(selected = false),
            ),
        )
        unsafeCases.forEachIndexed { index, context ->
            val store = PlannerStore(outboundState())
            val transport = FakeGoogleOutboundTransport()
            val coordinator = coordinator(
                store = store,
                transport = transport,
                accounts = accountState(context.account),
                imports = importState(context.collection),
            )
            assertTrue("unsafe target case $index", coordinator.targetsFor(ITEM_ID).isEmpty())
            assertEquals(
                "unsafe target case $index",
                GoogleCalendarOutboundOutcome.FAILED,
                coordinator.preparePreview(ITEM_ID, TARGET),
            )
            assertNull(store.state.value.pendingGoogleCalendarOutbound)
            assertTrue(transport.previewCalls.isEmpty())
        }

        val staleRevisionStore = PlannerStore(outboundState())
        val staleRevisionTransport = FakeGoogleOutboundTransport()
        val staleRevisionCoordinator = coordinator(
            store = staleRevisionStore,
            transport = staleRevisionTransport,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.FAILED,
            staleRevisionCoordinator.preparePreview(
                ITEM_ID,
                TARGET.copy(collectionRevision = COLLECTION_REVISION + 1),
            ),
        )
        assertNull(staleRevisionStore.state.value.pendingGoogleCalendarOutbound)
        assertTrue(staleRevisionTransport.previewCalls.isEmpty())
    }

    @Test
    fun approvalRequiresExactConfirmationAndPersistsEachOneShotStageBeforeNetwork() = runBlocking {
        val store = PlannerStore(outboundState())
        val transport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
        }
        val credentials = FakeGoogleOutboundCredentials()
        val coordinator = coordinator(
            store = store,
            transport = transport,
            credentials = credentials,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            coordinator.preparePreview(ITEM_ID, TARGET),
        )
        val confirmation = requireNotNull(coordinator.approvalConfirmation())

        val mismatches = listOf(
            confirmation(
                recoveryId = OTHER_RECOVERY_ID,
                operationGeneration = confirmation.operationGeneration,
                configurationId = confirmation.configurationId,
                previewId = confirmation.previewId,
                previewHash = confirmation.previewHash,
            ),
            confirmation(
                recoveryId = confirmation.recoveryId,
                operationGeneration = confirmation.operationGeneration + 1,
                configurationId = confirmation.configurationId,
                previewId = confirmation.previewId,
                previewHash = confirmation.previewHash,
            ),
            confirmation(
                recoveryId = confirmation.recoveryId,
                operationGeneration = confirmation.operationGeneration,
                configurationId = CONFIGURATION_B,
                previewId = confirmation.previewId,
                previewHash = confirmation.previewHash,
            ),
            confirmation(
                recoveryId = confirmation.recoveryId,
                operationGeneration = confirmation.operationGeneration,
                configurationId = confirmation.configurationId,
                previewId = OTHER_PREVIEW_ID,
                previewHash = confirmation.previewHash,
            ),
            confirmation(
                recoveryId = confirmation.recoveryId,
                operationGeneration = confirmation.operationGeneration,
                configurationId = confirmation.configurationId,
                previewId = confirmation.previewId,
                previewHash = OTHER_PREVIEW_HASH,
            ),
        )
        mismatches.forEachIndexed { index, mismatch ->
            assertEquals(
                "confirmation mismatch $index",
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                coordinator.approveAndEnqueue(mismatch),
            )
            assertEquals(
                GoogleCalendarOutboundStage.PREVIEWED,
                store.durableState.value?.pendingGoogleCalendarOutbound?.stage,
            )
            assertTrue(transport.approvalCalls.isEmpty())
            assertTrue(transport.enqueueCalls.isEmpty())
        }

        credentials.configurationId = CONFIGURATION_B
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            coordinator.approveAndEnqueue(confirmation),
        )
        assertEquals(
            GoogleCalendarOutboundStage.PREVIEWED,
            store.durableState.value?.pendingGoogleCalendarOutbound?.stage,
        )
        assertTrue(transport.approvalCalls.isEmpty())
        credentials.configurationId = CONFIGURATION_A

        transport.onApproval = { call ->
            val attempted = requireNotNull(
                store.durableState.value?.pendingGoogleCalendarOutbound,
            )
            assertEquals(GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED, attempted.stage)
            assertEquals(PREVIEW_ID, call.previewId)
            assertEquals(PREVIEW_HASH, call.expectedPreviewHash)
            validApproval()
        }
        transport.onEnqueue = { call ->
            val approved = requireNotNull(
                store.durableState.value?.pendingGoogleCalendarOutbound,
            )
            assertEquals(GoogleCalendarOutboundStage.APPROVED, approved.stage)
            assertEquals(CAPABILITY, approved.approvalCapability?.value)
            assertEquals(ACCOUNT_ID, call.accountId)
            assertEquals(COLLECTION_ID, call.collectionId)
            assertEquals(ITEM_ID, call.itemId)
            assertEquals(ITEM_REVISION, call.expectedItemRevision)
            assertEquals(CAPABILITY, call.approvalCapability)
            RemoteGoogleOutboundAccepted(OUTBOX_ID, replayed = false)
        }

        assertEquals(
            GoogleCalendarOutboundOutcome.ACCEPTED,
            coordinator.approveAndEnqueue(confirmation),
        )
        assertNull(store.durableState.value?.pendingGoogleCalendarOutbound)
        assertEquals(GoogleCalendarOutboundPhase.ACCEPTED, coordinator.state.value.phase)
        assertEquals(false, coordinator.state.value.acceptedWasReplay)
        assertEquals(1, transport.approvalCalls.size)
        assertEquals(1, transport.enqueueCalls.size)
        assertTrue(coordinator.resetPresentationWithoutRecovery())
        assertEquals(GoogleCalendarOutboundPhase.READY, coordinator.state.value.phase)
    }

    @Test
    fun ambiguousApprovalIsNeverRetriedAndRetainsAttemptMarker() = runBlocking {
        val store = PlannerStore(outboundState())
        val firstTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
            onApproval = { throw IOException("synthetic lost approval response") }
        }
        val first = coordinator(store = store, transport = firstTransport)
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            first.preparePreview(ITEM_ID, TARGET),
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.PENDING,
            first.approveAndEnqueue(requireNotNull(first.approvalConfirmation())),
        )
        assertEquals(
            GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED,
            store.durableState.value?.pendingGoogleCalendarOutbound?.stage,
        )

        val recoveryTransport = FakeGoogleOutboundTransport()
        val relaunched = coordinator(store = store, transport = recoveryTransport)
        assertEquals(GoogleCalendarOutboundOutcome.PENDING, relaunched.recoverPending())
        assertEquals(GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN, relaunched.state.value.phase)
        assertTrue(recoveryTransport.previewCalls.isEmpty())
        assertTrue(recoveryTransport.approvalCalls.isEmpty())
        assertTrue(recoveryTransport.enqueueCalls.isEmpty())
        assertNull(relaunched.approvalConfirmation())
    }

    @Test
    fun cancelledApprovalIsNeverRetriedAndRetainsAttemptMarker() = runBlocking {
        val store = PlannerStore(outboundState())
        val approvalEntered = CompletableDeferred<Unit>()
        val neverCompletes = CompletableDeferred<RemoteGoogleOutboundApproval>()
        val firstTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
            onApproval = {
                approvalEntered.complete(Unit)
                neverCompletes.await()
            }
        }
        val first = coordinator(store = store, transport = firstTransport)
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            first.preparePreview(ITEM_ID, TARGET),
        )
        val approval = async {
            first.approveAndEnqueue(requireNotNull(first.approvalConfirmation()))
        }
        withTimeout(3_000) { approvalEntered.await() }
        approval.cancelAndJoin()

        assertEquals(
            GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED,
            store.durableState.value?.pendingGoogleCalendarOutbound?.stage,
        )
        val recoveryTransport = FakeGoogleOutboundTransport()
        val relaunched = coordinator(store = store, transport = recoveryTransport)
        assertEquals(GoogleCalendarOutboundOutcome.PENDING, relaunched.recoverPending())
        assertTrue(recoveryTransport.approvalCalls.isEmpty())
        assertTrue(recoveryTransport.enqueueCalls.isEmpty())
    }

    @Test
    fun failedEnqueueRetainsApprovedCapabilityAndRecoveryReplaysExactAcceptedRequest() =
        runBlocking {
            val store = PlannerStore(outboundState())
            val firstTransport = FakeGoogleOutboundTransport().apply {
                onPreview = { validPreview() }
                onApproval = { validApproval() }
                onEnqueue = { throw IOException("synthetic lost enqueue response") }
            }
            val first = coordinator(store = store, transport = firstTransport)
            assertEquals(
                GoogleCalendarOutboundOutcome.PREVIEW_READY,
                first.preparePreview(ITEM_ID, TARGET),
            )
            assertEquals(
                GoogleCalendarOutboundOutcome.PENDING,
                first.approveAndEnqueue(requireNotNull(first.approvalConfirmation())),
            )
            val retained = requireNotNull(
                store.durableState.value?.pendingGoogleCalendarOutbound,
            )
            assertEquals(GoogleCalendarOutboundStage.APPROVED, retained.stage)
            assertEquals(CAPABILITY, retained.approvalCapability?.value)

            val recoveryTransport = FakeGoogleOutboundTransport().apply {
                onEnqueue = { call ->
                    assertEquals(retained.accountId, call.accountId)
                    assertEquals(retained.collectionId, call.collectionId)
                    assertEquals(retained.itemId, call.itemId)
                    assertEquals(retained.expectedItemRevision, call.expectedItemRevision)
                    assertEquals(retained.approvalCapability?.value, call.approvalCapability)
                    RemoteGoogleOutboundAccepted(OUTBOX_ID, replayed = true)
                }
            }
            val relaunched = coordinator(store = store, transport = recoveryTransport)
            assertEquals(GoogleCalendarOutboundOutcome.ACCEPTED, relaunched.recoverPending())
            assertNull(store.durableState.value?.pendingGoogleCalendarOutbound)
            assertEquals(true, relaunched.state.value.acceptedWasReplay)
            assertTrue(recoveryTransport.previewCalls.isEmpty())
            assertTrue(recoveryTransport.approvalCalls.isEmpty())
            assertEquals(1, recoveryTransport.enqueueCalls.size)
        }

    @Test
    fun everyRecoveryStageUsesOnlyItsPermittedNetworkTransition() = runBlocking {
        val intentStore = PlannerStore(outboundState(pending = intentJournal()))
        val intentTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
        }
        val intentCoordinator = coordinator(store = intentStore, transport = intentTransport)
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            intentCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundStage.PREVIEWED,
            intentStore.state.value.pendingGoogleCalendarOutbound?.stage,
        )
        assertEquals(1, intentTransport.previewCalls.size)
        assertTrue(intentTransport.approvalCalls.isEmpty())
        assertTrue(intentTransport.enqueueCalls.isEmpty())

        val previewedStore = PlannerStore(outboundState(pending = previewedJournal()))
        val previewedTransport = FakeGoogleOutboundTransport()
        val previewedCoordinator = coordinator(
            store = previewedStore,
            transport = previewedTransport,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            previewedCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundPhase.AWAITING_APPROVAL,
            previewedCoordinator.state.value.phase,
        )
        assertNotNull(previewedCoordinator.approvalConfirmation())
        assertNoNetworkCalls(previewedTransport)

        val attemptedStore = PlannerStore(outboundState(pending = attemptedJournal()))
        val attemptedTransport = FakeGoogleOutboundTransport()
        val attemptedCoordinator = coordinator(
            store = attemptedStore,
            transport = attemptedTransport,
        )
        assertEquals(GoogleCalendarOutboundOutcome.PENDING, attemptedCoordinator.recoverPending())
        assertEquals(
            GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN,
            attemptedCoordinator.state.value.phase,
        )
        assertNull(attemptedCoordinator.approvalConfirmation())
        assertNoNetworkCalls(attemptedTransport)

        val approvedStore = PlannerStore(outboundState(pending = approvedJournal()))
        val approvedTransport = FakeGoogleOutboundTransport().apply {
            onEnqueue = { RemoteGoogleOutboundAccepted(OUTBOX_ID, replayed = false) }
        }
        val approvedCoordinator = coordinator(
            store = approvedStore,
            transport = approvedTransport,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.ACCEPTED,
            approvedCoordinator.recoverPending(),
        )
        assertNull(approvedStore.state.value.pendingGoogleCalendarOutbound)
        assertTrue(approvedTransport.previewCalls.isEmpty())
        assertTrue(approvedTransport.approvalCalls.isEmpty())
        assertEquals(1, approvedTransport.enqueueCalls.size)
    }

    @Test
    fun recoveredIntentRequiresCurrentCandidateTargetAndExactCollectionRevision() = runBlocking {
        val unsafeContexts = listOf(
            outboundContext(account = accountSummary().copy(syncEnabled = false)),
            outboundContext(collection = writableCollection().copy(providerDeleted = true)),
        )
        unsafeContexts.forEachIndexed { index, context ->
            val store = PlannerStore(outboundState(pending = intentJournal()))
            val transport = FakeGoogleOutboundTransport().apply {
                onPreview = { validPreview() }
            }
            val coordinator = coordinator(
                store = store,
                transport = transport,
                accounts = accountState(context.account),
                imports = importState(context.collection),
            )

            assertEquals(
                "unsafe recovered intent case $index",
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                coordinator.recoverPending(),
            )
            assertEquals(
                GoogleCalendarOutboundStage.INTENT,
                store.state.value.pendingGoogleCalendarOutbound?.stage,
            )
            assertTrue("unsafe recovered intent case $index", transport.previewCalls.isEmpty())
        }

        val changedTargetStore = PlannerStore(outboundState(pending = intentJournal()))
        val changedTargetTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
        }
        val changedTargetCoordinator = coordinator(
            store = changedTargetStore,
            transport = changedTargetTransport,
            imports = importState(
                writableCollection().copy(revision = COLLECTION_REVISION + 1),
            ),
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            changedTargetCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundStage.INTENT,
            changedTargetStore.state.value.pendingGoogleCalendarOutbound?.stage,
        )
        assertEquals(1, changedTargetTransport.previewCalls.size)

        val invalidResponseStore = PlannerStore(outboundState(pending = intentJournal()))
        val invalidResponseTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview().copy(collectionRevision = COLLECTION_REVISION + 1) }
        }
        val invalidResponseCoordinator = coordinator(
            store = invalidResponseStore,
            transport = invalidResponseTransport,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            invalidResponseCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundStage.INTENT,
            invalidResponseStore.state.value.pendingGoogleCalendarOutbound?.stage,
        )
        assertEquals(1, invalidResponseTransport.previewCalls.size)

        val invalidWireStore = PlannerStore(outboundState(pending = intentJournal()))
        val invalidWireTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { throw GoogleCalendarOutboundApiException.InvalidResponse() }
        }
        val invalidWireCoordinator = coordinator(
            store = invalidWireStore,
            transport = invalidWireTransport,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            invalidWireCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundPhase.RECOVERY_REQUIRED,
            invalidWireCoordinator.state.value.phase,
        )
        assertEquals(
            GoogleCalendarOutboundStage.INTENT,
            invalidWireStore.state.value.pendingGoogleCalendarOutbound?.stage,
        )
    }

    @Test
    fun staleGoogleCacheBindingCannotPrepareRecoverIntentOrApprovePreview() = runBlocking {
        val staleBindings = listOf(
            accountState(configurationId = CONFIGURATION_B) to importState(),
            accountState() to importState(configurationId = CONFIGURATION_B),
        )
        staleBindings.forEachIndexed { index, (accounts, imports) ->
            val prepareStore = PlannerStore(outboundState())
            val prepareTransport = FakeGoogleOutboundTransport()
            val prepareCoordinator = coordinator(
                store = prepareStore,
                transport = prepareTransport,
                accounts = accounts,
                imports = imports,
            )
            assertEquals(
                "stale prepare cache case $index",
                GoogleCalendarOutboundOutcome.FAILED,
                prepareCoordinator.preparePreview(ITEM_ID, TARGET),
            )
            assertNull(prepareStore.state.value.pendingGoogleCalendarOutbound)
            assertNoNetworkCalls(prepareTransport)

            val intentStore = PlannerStore(outboundState(pending = intentJournal()))
            val intentTransport = FakeGoogleOutboundTransport().apply {
                onPreview = { validPreview() }
            }
            val intentCoordinator = coordinator(
                store = intentStore,
                transport = intentTransport,
                accounts = accounts,
                imports = imports,
            )
            assertEquals(
                "stale recovery cache case $index",
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                intentCoordinator.recoverPending(),
            )
            assertEquals(
                GoogleCalendarOutboundStage.INTENT,
                intentStore.state.value.pendingGoogleCalendarOutbound?.stage,
            )
            assertNoNetworkCalls(intentTransport)

            val previewedStore = PlannerStore(outboundState(pending = previewedJournal()))
            val approvalTransport = FakeGoogleOutboundTransport()
            val approvalCoordinator = coordinator(
                store = previewedStore,
                transport = approvalTransport,
                accounts = accounts,
                imports = imports,
            )
            assertEquals(
                GoogleCalendarOutboundOutcome.PREVIEW_READY,
                approvalCoordinator.recoverPending(),
            )
            assertNull(approvalCoordinator.approvalConfirmation())
            assertEquals(
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                approvalCoordinator.approveAndEnqueue(
                    GoogleCalendarOutboundApprovalConfirmation(
                        recoveryId = RECOVERY_ID,
                        operationGeneration = 1,
                        configurationId = CONFIGURATION_A,
                        previewId = PREVIEW_ID,
                        previewHash = PREVIEW_HASH,
                    ),
                ),
            )
            assertEquals(
                GoogleCalendarOutboundStage.PREVIEWED,
                previewedStore.state.value.pendingGoogleCalendarOutbound?.stage,
            )
            assertNoNetworkCalls(approvalTransport)
        }
    }

    @Test
    fun latePreviewResponseCannotCrossPrivacyOrCredentialBindingFence() = runBlocking {
        for (fence in LateResponseFence.entries) {
            val store = PlannerStore(outboundState())
            val credentials = FakeGoogleOutboundCredentials()
            var operationAllowed = true
            lateinit var coordinator: GoogleCalendarOutboundCoordinator
            val transport = FakeGoogleOutboundTransport().apply {
                onPreview = {
                    when (fence) {
                        LateResponseFence.PRIVACY -> operationAllowed = false
                        LateResponseFence.BINDING -> credentials.configurationId = CONFIGURATION_B
                    }
                    coordinator.quarantineBindingState()
                    validPreview()
                }
            }
            coordinator = coordinator(
                store = store,
                transport = transport,
                credentials = credentials,
                operationAllowed = { operationAllowed },
            )

            assertEquals(
                fence.name,
                GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
                coordinator.preparePreview(ITEM_ID, TARGET),
            )
            assertEquals(
                GoogleCalendarOutboundStage.INTENT,
                store.durableState.value?.pendingGoogleCalendarOutbound?.stage,
            )
            assertNull(store.state.value.pendingGoogleCalendarOutbound?.preview)
            assertNull(coordinator.state.value.preview)
            assertTrue(transport.approvalCalls.isEmpty())
            assertTrue(transport.enqueueCalls.isEmpty())
            if (fence == LateResponseFence.PRIVACY) {
                assertEquals(
                    GoogleCalendarOutboundPhase.PRIVACY_PROTECTED,
                    coordinator.state.value.phase,
                )
            }
        }
    }

    @Test
    fun lateApprovalAndEnqueueResponsesRemainAtTheirLastDurableSafeStage() = runBlocking {
        val approvalStore = PlannerStore(outboundState())
        var approvalAllowed = true
        lateinit var approvalCoordinator: GoogleCalendarOutboundCoordinator
        val approvalTransport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
            onApproval = {
                approvalAllowed = false
                approvalCoordinator.quarantineBindingState()
                validApproval()
            }
        }
        approvalCoordinator = coordinator(
            store = approvalStore,
            transport = approvalTransport,
            operationAllowed = { approvalAllowed },
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            approvalCoordinator.preparePreview(ITEM_ID, TARGET),
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            approvalCoordinator.approveAndEnqueue(
                requireNotNull(approvalCoordinator.approvalConfirmation()),
            ),
        )
        assertEquals(
            GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED,
            approvalStore.state.value.pendingGoogleCalendarOutbound?.stage,
        )
        assertNull(approvalStore.state.value.pendingGoogleCalendarOutbound?.approvalCapability)
        assertTrue(approvalTransport.enqueueCalls.isEmpty())
        assertEquals(
            GoogleCalendarOutboundPhase.PRIVACY_PROTECTED,
            approvalCoordinator.state.value.phase,
        )

        val enqueueStore = PlannerStore(outboundState(pending = approvedJournal()))
        val enqueueCredentials = FakeGoogleOutboundCredentials()
        lateinit var enqueueCoordinator: GoogleCalendarOutboundCoordinator
        val enqueueTransport = FakeGoogleOutboundTransport().apply {
            onEnqueue = {
                enqueueCredentials.configurationId = CONFIGURATION_B
                enqueueCoordinator.quarantineBindingState()
                RemoteGoogleOutboundAccepted(OUTBOX_ID, replayed = false)
            }
        }
        enqueueCoordinator = coordinator(
            store = enqueueStore,
            transport = enqueueTransport,
            credentials = enqueueCredentials,
        )
        assertEquals(
            GoogleCalendarOutboundOutcome.RECOVERY_REQUIRED,
            enqueueCoordinator.recoverPending(),
        )
        assertEquals(
            GoogleCalendarOutboundStage.APPROVED,
            enqueueStore.durableState.value?.pendingGoogleCalendarOutbound?.stage,
        )
        assertEquals(
            CAPABILITY,
            enqueueStore.durableState.value
                ?.pendingGoogleCalendarOutbound?.approvalCapability?.value,
        )
        assertEquals(1, enqueueTransport.enqueueCalls.size)
    }

    @Test
    fun expiredAuthorityCannotBeDiscardedUntilFiveMinuteSafetyWindowEnds() = runBlocking {
        var clock = Instant.parse("2026-09-02T12:24:59Z")
        val store = PlannerStore(outboundState(pending = attemptedJournal()))
        val transport = FakeGoogleOutboundTransport()
        val coordinator = coordinator(
            store = store,
            transport = transport,
            now = { clock },
        )

        assertEquals(GoogleCalendarOutboundOutcome.EXPIRED, coordinator.recoverPending())
        assertEquals(GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN, coordinator.state.value.phase)
        assertFalse(coordinator.discardExpiredRecovery())
        assertNotNull(store.state.value.pendingGoogleCalendarOutbound)

        clock = Instant.parse("2026-09-02T12:25:00Z")
        assertTrue(coordinator.discardExpiredRecovery())
        assertNull(store.durableState.value?.pendingGoogleCalendarOutbound)
        assertNoNetworkCalls(transport)
    }

    @Test
    fun coordinatorDiagnosticsRedactPrivateContentBindingsHashesAndCapabilities() = runBlocking {
        val store = PlannerStore(outboundState())
        val transport = FakeGoogleOutboundTransport().apply {
            onPreview = { validPreview() }
        }
        val coordinator = coordinator(store = store, transport = transport)
        val option = coordinator.targetsFor(ITEM_ID).single()
        assertEquals(
            GoogleCalendarOutboundOutcome.PREVIEW_READY,
            coordinator.preparePreview(ITEM_ID, option.target),
        )
        val journal = requireNotNull(store.state.value.pendingGoogleCalendarOutbound)
        val confirmation = requireNotNull(coordinator.approvalConfirmation())
        val diagnostics = listOf(
            option.toString(),
            option.target.toString(),
            coordinator.state.value.toString(),
            journal.toString(),
            requireNotNull(journal.preview).toString(),
            confirmation.toString(),
            GoogleCalendarOutboundApprovalCapability(CAPABILITY).toString(),
            validApproval().toString(),
        ).joinToString("\n")

        listOf(
            ACCOUNT_ID,
            COLLECTION_ID,
            ITEM_ID,
            PREVIEW_ID,
            PREVIEW_HASH,
            CAPABILITY,
            "Private focus",
            "Private calendar",
            "Private Gmail",
        ).forEach { secret -> assertFalse(secret, diagnostics.contains(secret)) }
    }

    private fun coordinator(
        store: PlannerStore,
        transport: FakeGoogleOutboundTransport,
        credentials: FakeGoogleOutboundCredentials = FakeGoogleOutboundCredentials(),
        accounts: GoogleAccountState = accountState(),
        imports: GoogleCalendarImportState = importState(),
        now: () -> Instant = { NOW },
        operationAllowed: () -> Boolean = { true },
    ) = GoogleCalendarOutboundCoordinator(
        plannerStore = store,
        credentialStore = credentials,
        transport = transport,
        googleAccountState = { accounts },
        googleImportState = { imports },
        now = now,
        newUuid = { UUID.fromString(RECOVERY_ID) },
        operationAllowed = operationAllowed,
    )

    private fun outboundState(
        item: CanonicalItemSnapshot = canonicalEvent(),
        pending: GoogleCalendarOutboundJournal? = null,
    ) = DayWeaveUiState(
        canonicalItems = listOf(item),
        canonicalSyncOrigin = API_BASE_URL,
        canonicalConfigurationId = CONFIGURATION_A,
        canonicalDeltaCursor = "cursor-1",
        pendingGoogleCalendarOutbound = pending,
    )

    private fun canonicalEvent(tentative: Boolean = false): CanonicalItemSnapshot {
        val timing = CanonicalEventTimingDraft(
            startsAt = "2026-09-02T10:00:00Z",
            endsAt = "2026-09-02T11:00:00Z",
            tentative = tentative,
        )
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.EVENT,
            title = "Private focus",
            timezoneName = "Europe/Paris",
            durationSeconds = 3_600,
            earliestStartAt = timing.startsAt,
            deadlineAt = timing.endsAt,
            eventTiming = timing,
        )
        return CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "event",
            status = "planned",
            title = draft.title,
            timezoneName = draft.timezoneName,
            durationSeconds = draft.durationSeconds,
            deadlineAt = draft.deadlineAt,
            earliestStartAt = draft.earliestStartAt,
            flexibleConstraintsJson = draft.constraints.toCanonicalJson(
                timing,
                draft.durationSeconds,
                draft.timezoneName,
            ).toString(),
            splitPolicyJson = draft.split.toCanonicalJson(draft.durationSeconds).toString(),
            importance = draft.importance,
            urgency = draft.urgency,
            siblingOrder = draft.siblingOrder,
            isExecutable = true,
            revision = ITEM_REVISION,
            createdAt = "2026-09-02T09:00:00Z",
            updatedAt = "2026-09-02T09:00:00Z",
        )
    }

    private fun accountSummary() = GoogleAccountSummary(
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
    )

    private fun accountState(
        account: GoogleAccountSummary = accountSummary(),
        configurationId: String = CONFIGURATION_A,
    ) =
        GoogleAccountState(
            phase = GoogleAccountPhase.CONNECTED,
            accounts = listOf(account),
            message = "Connected",
            configurationId = configurationId,
        )

    private fun writableCollection() = GoogleImportCollectionState(
        id = COLLECTION_ID,
        accountId = ACCOUNT_ID,
        displayName = "Private calendar",
        kind = RemoteGoogleCollectionKind.CALENDAR,
        providerDeleted = false,
        selected = true,
        visible = true,
        syncRole = RemoteGoogleSyncRole.WRITABLE,
        calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
        revision = COLLECTION_REVISION,
        lastImportAt = "2026-09-02T09:02:00Z",
        providerAccessRole = "owner",
    )

    private fun importState(
        collection: GoogleImportCollectionState = writableCollection(),
        configurationId: String = CONFIGURATION_A,
    ) =
        GoogleCalendarImportState(
            phase = GoogleCalendarImportPhase.READY,
            message = "Ready",
            accounts = mapOf(
                ACCOUNT_ID to GoogleImportAccountState(collections = listOf(collection)),
            ),
            configurationId = configurationId,
        )

    private fun intentJournal() = GoogleCalendarOutboundJournal(
        recoveryId = RECOVERY_ID,
        operationGeneration = 1,
        configurationId = CONFIGURATION_A,
        apiBaseUrl = API_BASE_URL,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        itemId = ITEM_ID,
        expectedItemRevision = ITEM_REVISION,
        intentExpiresAt = "2026-09-02T12:30:00Z",
        createdAt = NOW.toString(),
    )

    private fun previewedJournal() = intentJournal().recordingPreview(validPreview())

    private fun attemptedJournal() = previewedJournal().recordingApprovalAttempt()

    private fun approvedJournal() = attemptedJournal().recordingApproval(validApproval())

    private fun validPreview() = RemoteGoogleOutboundPreview(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = COLLECTION_REVISION,
        collectionDisplayName = "Private calendar",
        itemId = ITEM_ID,
        itemRevision = ITEM_REVISION,
        entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
        operation = GoogleCalendarOutboundOperation.UPSERT,
        providerResourceId = null,
        providerEtag = null,
        previewHash = PREVIEW_HASH,
        providerPayload = VALID_PAYLOAD,
        expiresAt = "2026-09-02T12:20:00Z",
    )

    private fun validApproval() = RemoteGoogleOutboundApproval(
        previewId = PREVIEW_ID,
        approvalCapability = CAPABILITY,
        expiresAt = "2026-09-02T12:14:00Z",
    )

    private fun assertNoNetworkCalls(transport: FakeGoogleOutboundTransport) {
        assertTrue(transport.previewCalls.isEmpty())
        assertTrue(transport.approvalCalls.isEmpty())
        assertTrue(transport.enqueueCalls.isEmpty())
    }

    private fun confirmation(
        recoveryId: String,
        operationGeneration: Long,
        configurationId: String,
        previewId: String,
        previewHash: String,
    ) = GoogleCalendarOutboundApprovalConfirmation(
        recoveryId = recoveryId,
        operationGeneration = operationGeneration,
        configurationId = configurationId,
        previewId = previewId,
        previewHash = previewHash,
    )

    private fun outboundContext(
        account: GoogleAccountSummary = accountSummary(),
        collection: GoogleImportCollectionState = writableCollection(),
    ) = OutboundContext(account, collection)

    private data class OutboundContext(
        val account: GoogleAccountSummary,
        val collection: GoogleImportCollectionState,
    )

    private enum class LateResponseFence {
        PRIVACY,
        BINDING,
    }

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-02T12:00:00Z")
        const val API_BASE_URL = "https://api.example.test/"
        const val CONFIGURATION_A = "configuration-a"
        const val CONFIGURATION_B = "configuration-b"
        const val RECOVERY_ID = "11111111-1111-4111-8111-111111111111"
        const val OTHER_RECOVERY_ID = "99999999-9999-4999-8999-999999999999"
        const val ACCOUNT_ID = "22222222-2222-4222-8222-222222222222"
        const val COLLECTION_ID = "33333333-3333-4333-8333-333333333333"
        const val ITEM_ID = "44444444-4444-4444-8444-444444444444"
        const val PREVIEW_ID = "55555555-5555-4555-8555-555555555555"
        const val OTHER_PREVIEW_ID = "66666666-6666-4666-8666-666666666666"
        const val OUTBOX_ID = "77777777-7777-4777-8777-777777777777"
        const val ITEM_REVISION = 7L
        const val COLLECTION_REVISION = 4L
        const val PREVIEW_HASH =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        const val OTHER_PREVIEW_HASH =
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        const val CAPABILITY = "dw_ga1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        val PROVIDER_EVENT_ID = "d1" + "a".repeat(64)
        const val OWNERSHIP_PROOF = "[server-managed]"
        val TARGET = GoogleCalendarOutboundTarget(
            accountId = ACCOUNT_ID,
            collectionId = COLLECTION_ID,
            collectionRevision = COLLECTION_REVISION,
        )
        val VALID_PAYLOAD: JsonObject = Json.parseToJsonElement(
            """
            {
              "id":"$PROVIDER_EVENT_ID",
              "etag":null,
              "summary":"Private focus",
              "description":"Private notes",
              "location":null,
              "status":"confirmed",
              "transparency":"opaque",
              "visibility":"private",
              "eventType":"default",
              "start":{"date":null,"dateTime":"2026-09-02T10:00:00+02:00","timeZone":"Europe/Paris"},
              "end":{"date":null,"dateTime":"2026-09-02T11:00:00+02:00","timeZone":"Europe/Paris"},
              "attendees":[],
              "attachments":[],
              "recurrence":[],
              "conferenceData":null,
              "recurringEventId":null,
              "originalStartTime":null,
              "updated":null,
              "sequence":null,
              "extendedProperties":{
                "private":{"dayweaveOwnershipProof":"$OWNERSHIP_PROOF"},
                "shared":{}
              }
            }
            """.trimIndent(),
        ) as JsonObject
    }
}

private class FakeGoogleOutboundCredentials : ApiCredentialStore {
    var configurationId: String = "configuration-a"
    var baseUrl: String = "https://api.example.test/"
    var enabled: Boolean = true

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = baseUrl,
        hasBearerToken = enabled,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? =
        if (enabled) {
            AuthenticatedApiConfiguration.createBound(
                baseUrl = baseUrl,
                bearerToken = "synthetic-google-outbound-token",
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

private data class PreviewCall(
    val accountId: String,
    val collectionId: String,
    val itemId: String,
    val expectedItemRevision: Long,
)

private data class ApprovalCall(
    val accountId: String,
    val previewId: String,
    val expectedPreviewHash: String,
)

private data class EnqueueCall(
    val accountId: String,
    val collectionId: String,
    val itemId: String,
    val expectedItemRevision: Long,
    val approvalCapability: String,
)

private class FakeGoogleOutboundTransport : GoogleCalendarOutboundTransport {
    val previewCalls = mutableListOf<PreviewCall>()
    val approvalCalls = mutableListOf<ApprovalCall>()
    val enqueueCalls = mutableListOf<EnqueueCall>()
    var onPreview: suspend (PreviewCall) -> RemoteGoogleOutboundPreview = {
        error("Unexpected Google Calendar preview")
    }
    var onApproval: suspend (ApprovalCall) -> RemoteGoogleOutboundApproval = {
        error("Unexpected Google Calendar approval")
    }
    var onEnqueue: suspend (EnqueueCall) -> RemoteGoogleOutboundAccepted = {
        error("Unexpected Google Calendar enqueue")
    }

    override suspend fun preview(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
    ): RemoteGoogleOutboundPreview {
        val call = PreviewCall(accountId, collectionId, itemId, expectedItemRevision)
        previewCalls += call
        return onPreview(call)
    }

    override suspend fun approve(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        previewId: String,
        expectedPreviewHash: String,
    ): RemoteGoogleOutboundApproval {
        val call = ApprovalCall(accountId, previewId, expectedPreviewHash)
        approvalCalls += call
        return onApproval(call)
    }

    override suspend fun enqueue(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        collectionId: String,
        itemId: String,
        expectedItemRevision: Long,
        approvalCapability: String,
    ): RemoteGoogleOutboundAccepted {
        val call = EnqueueCall(
            accountId,
            collectionId,
            itemId,
            expectedItemRevision,
            approvalCapability,
        )
        enqueueCalls += call
        return onEnqueue(call)
    }
}
