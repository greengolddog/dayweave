package com.greengolddog.dayweave.network

import java.io.IOException
import java.time.Instant
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class AccountRecoveryManagerTest {
    private val now = Instant.parse("2026-09-05T09:00:00Z")

    @Test
    fun onlyConsumptionAndUnreadableRecoveryStateBlockApiBoundMutationLanes() {
        val active = syntheticActiveState(now)
        val issuance = StoredAccountRecoveryJournal.IssuancePending(
            baseUrl = active.baseUrl,
            configurationId = active.session.id,
            clientInstanceId = active.clientInstanceId,
            candidateId = RECOVERY_ID,
            candidateCode = DeviceAuthSecret(
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 46),
            ),
            replacesId = null,
            replacesRevision = null,
            preparedAt = now.toString(),
        )
        val consumption = StoredAccountRecoveryJournal.ConsumptionPending(
            baseUrl = active.baseUrl,
            previousBaseUrl = active.baseUrl,
            previousBindingId = active.session.id,
            clientInstanceId = active.clientInstanceId,
            sessionId = "55555555-5555-4555-8555-555555555555",
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            preparedAt = now.toString(),
            recoveryCode = DeviceAuthSecret(
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 47),
            ),
            accessToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_ACCESS_TOKEN_PREFIX, 48),
            ),
            refreshToken = DeviceAuthSecret(
                syntheticDeviceToken(DEVICE_REFRESH_TOKEN_PREFIX, 49),
            ),
            successorId = "66666666-6666-4666-8666-666666666666",
            successorCode = DeviceAuthSecret(
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 50),
            ),
        )

        assertFalse(null.blocksApiBoundWork())
        assertFalse(issuance.blocksApiBoundWork())
        assertTrue(consumption.blocksApiBoundWork())
        assertTrue(
            StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation(
                baseUrl = consumption.baseUrl,
                previousBaseUrl = consumption.previousBaseUrl,
                previousBindingId = consumption.previousBindingId,
                clientInstanceId = consumption.clientInstanceId,
                session = syntheticSession(
                    now = now,
                    id = consumption.sessionId,
                    clientInstanceId = consumption.clientInstanceId,
                ),
                accessToken = consumption.accessToken,
                refreshToken = consumption.refreshToken,
                successorId = consumption.successorId,
                successorCode = consumption.successorCode,
                successorCreatedAt = now.toString(),
                successorRevision = 1,
            ).blocksApiBoundWork(),
        )
        assertTrue(
            StoredAccountRecoveryJournal.RepairRequired(
                reason = RECOVERY_JOURNAL_MALFORMED,
            ).blocksApiBoundWork(),
        )
    }

    @Test
    fun issuanceJournalAcceptsOnlyItsExactRetiredReauthBinding() {
        val active = syntheticActiveState(now)
        val journal = StoredAccountRecoveryJournal.IssuancePending(
            baseUrl = active.baseUrl,
            configurationId = active.session.id,
            clientInstanceId = active.clientInstanceId,
            candidateId = RECOVERY_ID,
            candidateCode = DeviceAuthSecret(
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 73),
            ),
            replacesId = null,
            replacesRevision = null,
            preparedAt = now.toString(),
        )
        val exact = StoredDeviceAuthState.Reauth(
            baseUrl = active.baseUrl,
            clientInstanceId = active.clientInstanceId,
            previousSessionId = active.session.id,
            reason = REAUTH_REFRESH_REJECTED,
        )

        validateStoredDeviceAuthEnvelopeContents(exact, journal)
        listOf(
            exact.copy(baseUrl = "https://other.example.test/"),
            exact.copy(clientInstanceId = "77777777-7777-4777-8777-777777777777"),
            exact.copy(previousSessionId = "88888888-8888-4888-8888-888888888888"),
        ).forEach { mismatched ->
            assertThrows(IllegalArgumentException::class.java) {
                validateStoredDeviceAuthEnvelopeContents(mismatched, journal)
            }
        }
    }

    @Test
    fun issueJournalsThenDisclosesWithoutPuttingPlaintextInStateAndAckClears() = runBlocking {
        val fixture = fixture()
        fixture.transport.currentResponse = CurrentAccountRecoveryCodeResponse(null)
        fixture.manager.refresh()
        assertEquals(AccountRecoveryPhase.READY, fixture.manager.state.value.phase)
        val confirmation = requireNotNull(fixture.manager.issuanceConfirmation())
        fixture.transport.issueHandler = { _, request, _ ->
            AccountRecoveryCodeMutationResponse(
                AccountRecoveryCodeContract(request.id, now.toString(), 1),
                replayed = false,
            )
        }

        assertEquals(
            DeviceAuthActionResult.SUCCESS,
            fixture.manager.issueOrRotate(confirmation),
        )

        val journal = fixture.store.envelope.accountRecoveryJournal
        assertTrue(journal is StoredAccountRecoveryJournal.DisclosurePending)
        val disclosure = requireNotNull(fixture.manager.disclosure())
        assertFalse(fixture.manager.state.value.toString().contains(disclosure.code))
        assertFalse(fixture.manager.toString().contains(disclosure.code))
        assertTrue(fixture.manager.acknowledge(disclosure))
        assertNull(fixture.store.envelope.accountRecoveryJournal)
        assertNull(fixture.manager.disclosure())
    }

    @Test
    fun issuanceNetworkFailureReplaysExactPersistedTupleAfterRestart() = runBlocking {
        val fixture = fixture()
        fixture.manager.refresh()
        val confirmation = requireNotNull(fixture.manager.issuanceConfirmation())
        fixture.transport.issueHandler = { _, _, _ -> throw IOException("lost response") }

        assertEquals(
            DeviceAuthActionResult.PENDING_RETRY,
            fixture.manager.issueOrRotate(confirmation),
        )
        val pending = fixture.store.envelope.accountRecoveryJournal as
            StoredAccountRecoveryJournal.IssuancePending
        val restartedTransport = RecordingAccountRecoveryTransport().apply {
            issueHandler = { _, request, _ ->
                AccountRecoveryCodeMutationResponse(
                    AccountRecoveryCodeContract(request.id, now.toString(), 1),
                    replayed = true,
                )
            }
        }
        val restarted = fixture.manager(transport = restartedTransport)

        assertEquals(DeviceAuthActionResult.SUCCESS, restarted.retryPending())
        assertEquals(pending.candidateId, restartedTransport.issueCalls.single().request.id)
        assertEquals(
            pending.candidateCode.value,
            restartedTransport.issueCalls.single().request.recoveryCode,
        )
    }

    @Test
    fun issuance401RetainsJournalUntilExactOwnerConfirmedDiscard() = runBlocking {
        val fixture = fixture()
        fixture.manager.refresh()
        fixture.transport.issueHandler = { _, _, _ ->
            throw DeviceAuthApiException.Authentication()
        }

        assertEquals(
            DeviceAuthActionResult.AUTH_REQUIRED,
            fixture.manager.issueOrRotate(
                requireNotNull(fixture.manager.issuanceConfirmation()),
            ),
        )
        assertTrue(
            fixture.store.envelope.accountRecoveryJournal is
                StoredAccountRecoveryJournal.IssuancePending,
        )
        assertTrue(fixture.manager.state.value.retryAvailable)
        assertTrue(fixture.manager.state.value.discardAvailable)
        val discard = requireNotNull(fixture.manager.journalDiscardConfirmation())

        assertTrue(fixture.manager.discardJournal(discard))
        assertNull(fixture.store.envelope.accountRecoveryJournal)
        requireNotNull(fixture.coordinator.authenticatedConfiguration())
        assertFalse(fixture.manager.discardJournal(discard))
    }

    @Test
    fun consumeFailureSuppressesOldAuthorizationAndExactRetryAtomicallyInstallsSuccessor() =
        runBlocking {
            val fixture = fixture()
            val recoveryCode = syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 41)
            fixture.transport.consumeHandler = { _, _, _, _ -> throw IOException("offline") }

            assertEquals(
                DeviceAuthActionResult.PENDING_RETRY,
                fixture.manager.consume(SYNTHETIC_BASE_URL, recoveryCode, confirmed = true),
            )
            val pending = fixture.store.envelope.accountRecoveryJournal as
                StoredAccountRecoveryJournal.ConsumptionPending
            assertFalse(fixture.coordinator.snapshot().hasBearerToken)
            assertNull(fixture.coordinator.authenticatedConfiguration())
            assertTrue(fixture.manager.state.value.deviceAuthorizationSuppressed)

            val restartedTransport = RecordingAccountRecoveryTransport().apply {
                consumeHandler = { _, _, request, _ -> successfulConsumption(request, replayed = true) }
            }
            val restarted = fixture.manager(transport = restartedTransport)

            assertEquals(DeviceAuthActionResult.SUCCESS, restarted.retryPending())
            val installed = fixture.store.envelope.state as StoredDeviceAuthState.Active
            assertEquals(pending.sessionId, installed.session.id)
            assertEquals(pending.clientInstanceId, installed.clientInstanceId)
            assertEquals(pending.accessToken, installed.accessToken)
            val successor = fixture.store.envelope.accountRecoveryJournal as
                StoredAccountRecoveryJournal.DisclosurePending
            assertEquals(pending.successorId, successor.id)
            assertEquals(pending.successorCode, successor.code)
            assertEquals(1, fixture.fence.calls.size)
            requireNotNull(fixture.coordinator.authenticatedConfiguration())
            Unit
        }

    @Test
    fun consume401RetainsTupleAndOldAuthorizationRemainsSuppressedUntilDiscard() = runBlocking {
        val fixture = fixture()
        fixture.transport.consumeHandler = { _, _, _, _ ->
            throw DeviceAuthApiException.Authentication()
        }

        assertEquals(
            DeviceAuthActionResult.AUTH_REQUIRED,
            fixture.manager.consume(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 42),
                confirmed = true,
            ),
        )
        assertTrue(
            fixture.store.envelope.accountRecoveryJournal is
                StoredAccountRecoveryJournal.ConsumptionPending,
        )
        assertFalse(fixture.coordinator.snapshot().hasBearerToken)
        assertTrue(fixture.manager.state.value.deviceAuthorizationSuppressed)
        val confirmation = requireNotNull(fixture.manager.journalDiscardConfirmation())

        assertTrue(fixture.manager.discardJournal(confirmation))
        assertTrue(fixture.coordinator.snapshot().hasBearerToken)
        requireNotNull(fixture.coordinator.authenticatedConfiguration())
        Unit
    }

    @Test
    fun invalidConsumptionResponseRetainsPendingTupleAndNeverRecordsCommitEvidence() = runBlocking {
        val fixture = fixture()
        fixture.transport.consumeHandler = { _, _, _, _ ->
            throw DeviceAuthApiException.InvalidResponse()
        }

        assertEquals(
            DeviceAuthActionResult.SERVER_REJECTED,
            fixture.manager.consume(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 52),
                confirmed = true,
            ),
        )

        assertTrue(
            fixture.store.envelope.accountRecoveryJournal is
                StoredAccountRecoveryJournal.ConsumptionPending,
        )
        assertFalse(
            fixture.store.envelope.accountRecoveryJournal is
                StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation,
        )
        assertTrue(fixture.manager.state.value.retryAvailable)
        assertTrue(fixture.manager.state.value.discardAvailable)
        assertTrue(fixture.manager.state.value.deviceAuthorizationSuppressed)
        assertFalse(fixture.coordinator.snapshot().hasBearerToken)
    }

    @Test
    fun validatedCommitSurvivesFenceFailureAndRestartInstallsWithoutRecoveryBearer() = runBlocking {
        val fixture = fixture()
        fixture.fence.allowed = false
        fixture.transport.consumeHandler = { _, _, request, _ ->
            successfulConsumption(request, replayed = false)
        }

        assertEquals(
            DeviceAuthActionResult.CACHE_FENCE_BLOCKED,
            fixture.manager.consume(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 51),
                confirmed = true,
            ),
        )

        val committed = fixture.store.envelope.accountRecoveryJournal as
            StoredAccountRecoveryJournal.ConsumptionCommittedAwaitingInstallation
        assertFalse(fixture.manager.state.value.discardAvailable)
        assertNull(fixture.manager.journalDiscardConfirmation())
        assertFalse(fixture.coordinator.snapshot().hasBearerToken)
        assertEquals(1, fixture.transport.consumeCalls.size)
        fixture.fence.allowed = true
        val restarted = fixture.manager(transport = fixture.transport)

        assertEquals(DeviceAuthActionResult.SUCCESS, restarted.retryPending())
        assertEquals(1, fixture.transport.consumeCalls.size)
        val installed = fixture.store.envelope.state as StoredDeviceAuthState.Active
        assertEquals(committed.session, installed.session)
        assertEquals(committed.clientInstanceId, installed.clientInstanceId)
        assertEquals(committed.accessToken, installed.accessToken)
        assertEquals(committed.refreshToken, installed.refreshToken)
        val disclosure = fixture.store.envelope.accountRecoveryJournal as
            StoredAccountRecoveryJournal.DisclosurePending
        assertEquals(committed.successorId, disclosure.id)
        assertEquals(committed.successorCode, disclosure.code)
        assertEquals(committed.successorCreatedAt, disclosure.createdAt)
        assertEquals(committed.successorRevision, disclosure.revision)
    }

    @Test
    fun preflightBlockerPreventsJournalAndTransportWithoutChangingCredentials() = runBlocking {
        val fixture = fixture()
        val before = fixture.store.envelope
        fixture.fence.accountRecoveryPreflightAllowed = false

        val result = fixture.manager.consume(
            SYNTHETIC_BASE_URL,
            syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 44),
            confirmed = true,
        )

        assertEquals(DeviceAuthActionResult.NOT_ALLOWED, result)
        assertEquals(before.state, fixture.store.envelope.state)
        assertNull(fixture.store.envelope.accountRecoveryJournal)
        assertTrue(fixture.transport.consumeCalls.isEmpty())
        assertEquals(1, fixture.fence.accountRecoveryPreflightCalls.size)
    }

    @Test
    fun consumeWriterDrainsPriorReaderAndRejectsNewReaderUntilAtomicInstall() = runBlocking {
        val fixture = fixture()
        val priorEntered = CompletableDeferred<Unit>()
        val releasePrior = CompletableDeferred<Unit>()
        val transportEntered = CompletableDeferred<Unit>()
        val releaseTransport = CompletableDeferred<Unit>()
        val readerGeneration = fixture.gate.captureGeneration()
        val prior = async {
            fixture.gate.withOperation(readerGeneration) {
                priorEntered.complete(Unit)
                releasePrior.await()
            }
        }
        priorEntered.await()
        fixture.transport.consumeHandler = { _, _, request, _ ->
            transportEntered.complete(Unit)
            releaseTransport.await()
            successfulConsumption(request, replayed = false)
        }
        val consume = async {
            fixture.manager.consume(
                SYNTHETIC_BASE_URL,
                syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 45),
                confirmed = true,
            )
        }
        yield()
        assertFalse(transportEntered.isCompleted)

        releasePrior.complete(Unit)
        prior.await()
        transportEntered.await()
        val crossed = CompletableDeferred<Unit>()
        val lateReader = async {
            runCatching {
                fixture.gate.withOperation(readerGeneration) { crossed.complete(Unit) }
            }.exceptionOrNull()
        }
        yield()
        assertFalse(crossed.isCompleted)

        releaseTransport.complete(Unit)
        assertEquals(DeviceAuthActionResult.SUCCESS, consume.await())
        assertTrue(lateReader.await() is ApiBindingChangedException)
        assertFalse(crossed.isCompleted)
    }

    @Test
    fun repairRequiredFailsClosedAndConfirmedRemovalPreservesDeviceSession() = runBlocking {
        val active = syntheticActiveState(now)
        val repair = StoredAccountRecoveryJournal.RepairRequired(
            baseUrl = active.baseUrl,
            reason = RECOVERY_JOURNAL_UNSUPPORTED,
        )
        val fixture = fixture(active, repair)

        fixture.manager.refresh()

        assertEquals(AccountRecoveryPhase.REPAIR_REQUIRED, fixture.manager.state.value.phase)
        assertTrue(fixture.manager.state.value.deviceAuthorizationSuppressed)
        assertFalse(fixture.coordinator.snapshot().hasBearerToken)
        val confirmation = requireNotNull(fixture.manager.journalDiscardConfirmation())
        assertTrue(confirmation.repairsUnreadableState)
        assertTrue(fixture.manager.discardJournal(confirmation))
        assertEquals(active, fixture.store.envelope.state)
        assertNull(fixture.store.envelope.accountRecoveryJournal)
        assertTrue(fixture.coordinator.snapshot().hasBearerToken)
    }

    @Test
    fun privacyBoundaryInvalidatesDisclosureAndStaleAcknowledgement() = runBlocking {
        val disclosureJournal = StoredAccountRecoveryJournal.DisclosurePending(
            baseUrl = SYNTHETIC_BASE_URL,
            id = RECOVERY_ID,
            code = DeviceAuthSecret(syntheticDeviceToken(ACCOUNT_RECOVERY_TOKEN_PREFIX, 43)),
            createdAt = now.toString(),
            revision = 1,
            source = "issued",
        )
        val fixture = fixture(initialJournal = disclosureJournal)
        val disclosure = requireNotNull(fixture.manager.disclosure())

        fixture.manager.quarantineForPrivacyBoundary()

        assertEquals(AccountRecoveryPhase.LOCKED, fixture.manager.state.value.phase)
        assertNull(fixture.manager.disclosure())
        assertFalse(fixture.manager.acknowledge(disclosure))
        assertEquals(disclosureJournal, fixture.store.envelope.accountRecoveryJournal)
    }

    private fun fixture(
        state: StoredDeviceAuthState = syntheticActiveState(now),
        initialJournal: StoredAccountRecoveryJournal? = null,
    ): Fixture {
        val store = FakeDeviceAuthEnvelopeStore(state, initialJournal)
        val authTransport = RecordingDeviceAuthTransport()
        val gate = ApiBindingOperationGate()
        val fence = RecordingDeviceAuthFence()
        val coordinator = DurableDeviceAuthCoordinator(
            store = store,
            transport = authTransport,
            clientVersion = SYNTHETIC_CLIENT_VERSION,
            deviceLabel = SYNTHETIC_DEVICE_LABEL,
            bindingOperationGate = gate,
            bindingFence = fence,
            now = { now },
            generator = QueueDeviceCredentialGenerator(),
        )
        val transport = RecordingAccountRecoveryTransport().apply {
            currentResponse = CurrentAccountRecoveryCodeResponse(null)
        }
        val generator = QueueDeviceCredentialGenerator().apply {
            enqueueSessionId("55555555-5555-4555-8555-555555555555")
            enqueueSessionId("66666666-6666-4666-8666-666666666666")
            enqueueSessionId("77777777-7777-4777-8777-777777777777")
        }
        val factory: (AccountRecoveryTransport) -> AccountRecoveryManager = { selectedTransport ->
            AccountRecoveryManager(
                store = store,
                credentialStore = coordinator,
                transport = selectedTransport,
                bindingOperationGate = gate,
                bindingFence = fence,
                deviceLabel = SYNTHETIC_DEVICE_LABEL,
                clientVersion = SYNTHETIC_CLIENT_VERSION,
                generator = generator,
                now = { now },
            )
        }
        return Fixture(store, coordinator, transport, fence, gate, factory, factory(transport))
    }

    private fun successfulConsumption(
        request: ConsumeAccountRecoveryCodeRequest,
        replayed: Boolean,
    ) = AccountRecoveryConsumptionResponse(
        session = syntheticSession(
            now = now,
            id = request.sessionId,
            clientInstanceId = request.clientInstanceId,
        ),
        successorRecoveryCode = AccountRecoveryCodeContract(
            request.successorRecoveryCodeId,
            now.toString(),
            1,
        ),
        replayed = replayed,
    )

    private data class Fixture(
        val store: FakeDeviceAuthEnvelopeStore,
        val coordinator: DurableDeviceAuthCoordinator,
        val transport: RecordingAccountRecoveryTransport,
        val fence: RecordingDeviceAuthFence,
        val gate: ApiBindingOperationGate,
        val managerFactory: (AccountRecoveryTransport) -> AccountRecoveryManager,
        val manager: AccountRecoveryManager,
    ) {
        fun manager(transport: AccountRecoveryTransport): AccountRecoveryManager =
            managerFactory(transport)
    }

    private companion object {
        const val RECOVERY_ID = "88888888-8888-4888-8888-888888888888"
    }
}

private class RecordingAccountRecoveryTransport : AccountRecoveryTransport {
    var currentResponse = CurrentAccountRecoveryCodeResponse(null)
    val issueCalls = mutableListOf<IssueCall>()
    val consumeCalls = mutableListOf<ConsumeRecoveryCall>()
    var issueHandler: suspend (
        AuthenticatedApiConfiguration,
        CreateAccountRecoveryCodeRequest,
        Instant,
    ) -> AccountRecoveryCodeMutationResponse = { _, _, _ ->
        throw IOException("synthetic issue response not configured")
    }
    var consumeHandler: suspend (
        String,
        String,
        ConsumeAccountRecoveryCodeRequest,
        Instant,
    ) -> AccountRecoveryConsumptionResponse = { _, _, _, _ ->
        throw IOException("synthetic consume response not configured")
    }

    override suspend fun current(
        configuration: AuthenticatedApiConfiguration,
    ): CurrentAccountRecoveryCodeResponse = currentResponse

    override suspend fun issue(
        configuration: AuthenticatedApiConfiguration,
        request: CreateAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryCodeMutationResponse {
        issueCalls += IssueCall(configuration, request, preparedAt)
        return issueHandler(configuration, request, preparedAt)
    }

    override suspend fun consume(
        baseUrl: String,
        recoveryCode: String,
        request: ConsumeAccountRecoveryCodeRequest,
        preparedAt: Instant,
    ): AccountRecoveryConsumptionResponse {
        consumeCalls += ConsumeRecoveryCall(baseUrl, recoveryCode, request, preparedAt)
        return consumeHandler(baseUrl, recoveryCode, request, preparedAt)
    }

    data class IssueCall(
        val configuration: AuthenticatedApiConfiguration,
        val request: CreateAccountRecoveryCodeRequest,
        val preparedAt: Instant,
    )

    data class ConsumeRecoveryCall(
        val baseUrl: String,
        val recoveryCode: String,
        val request: ConsumeAccountRecoveryCodeRequest,
        val preparedAt: Instant,
    )
}
